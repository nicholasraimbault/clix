use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;

use serde_json::Value;

use crate::error::{ClixError, Result};
use crate::store::Store;
use crate::types::{BodyId, Grant, Job, JobStatus};
use crate::{grant, job, request};

/// Execute the granted path directly, preserving argv and bytes.
pub fn run_granted(grant: &Grant, argv: &[String]) -> Result<Output> {
    Ok(command(grant, argv)?.output()?)
}

fn command(grant: &Grant, argv: &[String]) -> Result<Command> {
    validate_argv(grant, argv)?;
    let mut command = Command::new(&grant.binary);
    command.arg0(&grant.tool).args(&argv[1..]);
    Ok(command)
}

fn validate_argv(grant: &Grant, argv: &[String]) -> Result<()> {
    if argv.first() != Some(&grant.tool) {
        return Err(ClixError::Usage("argv[0] is not granted".into()));
    }
    Ok(())
}

/// Persist acceptance and reserve permission before starting a process. Duplicate
/// delivery of the same invocation returns its existing state and never reruns it.
pub(crate) fn submit(
    store: &Arc<Mutex<Store>>,
    limits: &crate::limits::Limits,
    from: &BodyId,
    argv: &[String],
    id: String,
) -> Result<Value> {
    submit_inner(store, limits, from, argv, id, None)
}

pub(crate) fn submit_certified(
    store: &Arc<Mutex<Store>>,
    limits: &crate::limits::Limits,
    certificate: &crate::history::Certificate,
) -> Result<Value> {
    let i = &certificate.invocation;
    submit_inner(
        store,
        limits,
        &i.from,
        &i.argv,
        i.id.clone(),
        Some(certificate),
    )
}

fn submit_inner(
    store: &Arc<Mutex<Store>>,
    limits: &crate::limits::Limits,
    from: &BodyId,
    argv: &[String],
    id: String,
    certificate: Option<&crate::history::Certificate>,
) -> Result<Value> {
    crate::limits::invocation(&id, argv)?;
    let (accepted, grant) = {
        let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(certificate) = certificate {
            crate::history::validate_invocation(&s, certificate)?;
            if certificate.invocation.legacy_observer.is_some()
                || certificate.invocation.body.0 != s.body_name
            {
                return Err(ClixError::Protocol(
                    "invalid certified execution destination".into(),
                ));
            }
        }
        if let Some(existing) = s.jobs.iter().find(|j| j.id == id) {
            if existing.from != *from || existing.body.0 != s.body_name || existing.argv != argv {
                return Err(ClixError::Protocol(
                    "job ID belongs to a different invocation".into(),
                ));
            }
            if let Some(c) = certificate {
                if s.job_certificates.get(&id) != Some(&crate::history::invocation_hash(c)?) {
                    return Err(ClixError::Protocol(
                        "job ID belongs to a different signed admission".into(),
                    ));
                }
            }
            let local_queue = from.0 == s.body_name
                && matches!(
                    existing.status,
                    JobStatus::Queued | JobStatus::WaitingCapacity
                );
            if !local_queue {
                return Ok(job::exec_json(existing));
            }
        }
        s.update(|s| {
            if let Some(c) = certificate {
                crate::history::bind_admission(s, c)?;
            }
            let mut j = Job {
                id: id.clone(),
                body: BodyId(s.body_name.clone()),
                from: from.clone(),
                argv: argv.to_vec(),
                status: JobStatus::Running,
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
            let granted = match grant::check(s, &argv[0], from) {
                Ok(g) => {
                    let permit = limits.child()?;
                    if g.once {
                        s.grants
                            .iter_mut()
                            .find(|x| x.tool == g.tool)
                            .unwrap()
                            .reservation = Some(id.clone());
                    }
                    Some((g, permit))
                }
                Err(e) => {
                    if !matches!(e, ClixError::GrantBusy { .. }) {
                        request::upsert(s, from.clone(), &argv[0])?;
                    }
                    j.status = JobStatus::Denied {
                        reason: e.to_string(),
                    };
                    None
                }
            };
            if let Some(existing) = s.jobs.iter_mut().find(|existing| existing.id == id) {
                *existing = j.clone();
            } else {
                s.jobs.push(j.clone());
            }
            Ok((j, granted))
        })?
    };
    if let Some((grant, permit)) = grant {
        let store = store.clone();
        let run = accepted.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let output = run_async(&grant, &run.argv).await;
            let id = run.id.clone();
            let result = finish(&store, &grant, run, output);
            if let Err(e) = result {
                let reason = format!("could not persist job completion: {e}; its outcome is uncertain. Not rerunning.");
                eprintln!("{reason}");
                // Durable state still says Running, so restart recovery also
                // reports uncertainty. Keep the reserved grant fail-closed.
                let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                    j.status = JobStatus::Uncertain { reason };
                }
            }
        });
    }
    Ok(job::exec_json(&accepted))
}

fn finish(
    store: &Arc<Mutex<Store>>,
    grant: &Grant,
    mut run: Job,
    output: RunOutcome,
) -> Result<()> {
    let success = match &output {
        RunOutcome::Exited { output, .. } => Some(output.status.success()),
        RunOutcome::NotStarted(_) => Some(false),
        RunOutcome::Uncertain(_) => None,
    };
    match output {
        RunOutcome::Exited {
            output: out,
            capture_error,
        } => {
            let exit = out
                .status
                .code()
                .unwrap_or_else(|| 128 + out.status.signal().unwrap_or(1));
            run.status = match capture_error {
                None => JobStatus::Done { exit },
                Some(reason) => JobStatus::Failed {
                    reason: format!(
                        "{} exited {exit}, but output capture failed: {reason}",
                        grant.tool
                    ),
                },
            };
            run.stdout = out.stdout;
            run.stderr = out.stderr;
        }
        RunOutcome::NotStarted(e) => {
            run.status = JobStatus::Failed {
                reason: format!("could not run {}: {e}", grant.tool),
            }
        }
        RunOutcome::Uncertain(e) => {
            run.status = JobStatus::Uncertain {
                reason: format!(
                    "could not determine {} exit status: {e}. Not rerunning.",
                    grant.tool
                ),
            };
        }
    }
    let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
    s.update(|s| {
        // A replacement grant is a new owner decision; an older run cannot consume it.
        if grant.once {
            if let Some(index) = s
                .grants
                .iter()
                .position(|g| g.tool == grant.tool && g.reservation.as_deref() == Some(&run.id))
            {
                if success == Some(true) {
                    s.grants.remove(index);
                } else if success == Some(false) {
                    s.grants[index].reservation = None;
                }
            }
        }
        let existing = s
            .jobs
            .iter_mut()
            .find(|j| j.id == run.id)
            .ok_or_else(|| ClixError::Protocol("accepted job disappeared".into()))?;
        *existing = run;
        Ok(())
    })
}

const OUTPUT_LIMIT: u64 = crate::limits::OUTPUT_STREAM_BYTES;
enum RunOutcome {
    Exited {
        output: Output,
        capture_error: Option<String>,
    },
    NotStarted(std::io::Error),
    Uncertain(std::io::Error),
}
async fn read_output(reader: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err(std::io::Error::other(
            "command output exceeds 4 MiB per stream; stopped the command",
        ));
    }
    Ok(bytes)
}
async fn run_async(grant: &Grant, argv: &[String]) -> RunOutcome {
    let command = match command(grant, argv) {
        Ok(command) => command,
        Err(e) => return RunOutcome::NotStarted(std::io::Error::other(e.to_string())),
    };
    let mut child = match tokio::process::Command::from(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return RunOutcome::NotStarted(e),
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let streams = tokio::try_join!(read_output(stdout), read_output(stderr));
    let (stdout, stderr, capture_error) = match streams {
        Ok((stdout, stderr)) => (stdout, stderr, None),
        Err(e) => {
            let _ = child.start_kill();
            (Vec::new(), Vec::new(), Some(e.to_string()))
        }
    };
    match child.wait().await {
        Ok(status) => RunOutcome::Exited {
            output: Output {
                status,
                stdout,
                stderr,
            },
            capture_error,
        },
        Err(e) => RunOutcome::Uncertain(e),
    }
}

#[cfg(test)]
mod resource_tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    fn grant(tool: &str, binary: PathBuf, once: bool) -> Grant {
        Grant {
            tool: tool.into(),
            binary,
            allow_from: None,
            once,
            until: None,
            schedule: None,
            reservation: None,
        }
    }

    fn fixture(dir: &Path) -> PathBuf {
        let path = dir.join("gate-fixture");
        fs::write(&path, b"#!/bin/sh\nprintf 'started\\n' >> \"$1\"\nwhile [ ! -e \"$2\" ]; do sleep 0.01; done\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    async fn wait_for(mut check: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(Instant::now() < until, "fixture deadline exceeded");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn count(path: &Path) -> usize {
        fs::read_to_string(path).unwrap_or_default().lines().count()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runtime_child_limit_precedes_once_reservation_and_releases_after_completion() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fixture(dir.path());
        let marker = dir.path().join("markers");
        let gate = dir.path().join("release");
        let mut s = Store::open(&dir.path().join("state")).unwrap();
        s.body_name = "runner".into();
        s.grants.push(grant("gate", binary.clone(), false));
        s.grants.push(grant("once-gate", binary, true));
        // Learned/displayed Running rows do not consume runtime execution permits.
        for n in 0..20 {
            s.jobs.push(Job {
                id: format!("display-only-{n}"),
                body: BodyId("runner".into()),
                from: BodyId("origin".into()),
                argv: vec!["gate".into()],
                status: JobStatus::Running,
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        }
        s.save().unwrap();
        let s = Arc::new(Mutex::new(s));
        let limits = crate::limits::Limits::default();
        let from = BodyId("origin".into());
        let mut argv = vec![
            "gate".into(),
            marker.to_string_lossy().into(),
            gate.to_string_lossy().into(),
        ];
        for n in 0..crate::limits::CHILDREN {
            submit(&s, &limits, &from, &argv, format!("accepted-{n}")).unwrap();
        }
        wait_for(|| count(&marker) == crate::limits::CHILDREN).await;
        argv[0] = "once-gate".into();
        assert!(matches!(
            submit(&s, &limits, &from, &argv, "fifth".into()),
            Err(ClixError::Capacity(_))
        ));
        {
            let s = s.lock().unwrap();
            assert!(s.ensure_writable().is_ok());
            assert!(!s.jobs.iter().any(|j| j.id == "fifth"));
            assert!(s
                .grants
                .iter()
                .find(|g| g.tool == "once-gate")
                .unwrap()
                .reservation
                .is_none());
        }
        assert_eq!(count(&marker), 4);
        fs::write(&gate, []).unwrap();
        wait_for(|| {
            s.lock()
                .unwrap()
                .jobs
                .iter()
                .filter(|j| j.id.starts_with("accepted-"))
                .all(|j| matches!(j.status, JobStatus::Done { exit: 0 }))
        })
        .await;
        // Completion publishes immediately before its permit drops; allow that handoff.
        wait_for(|| match submit(&s, &limits, &from, &argv, "fifth".into()) {
            Ok(_) => true,
            Err(ClixError::Capacity(_)) => false,
            Err(e) => panic!("unexpected admission error: {e}"),
        })
        .await;
        wait_for(|| {
            s.lock()
                .unwrap()
                .jobs
                .iter()
                .any(|j| j.id == "fifth" && matches!(j.status, JobStatus::Done { exit: 0 }))
        })
        .await;
        assert_eq!(count(&marker), 5);
        assert!(!s
            .lock()
            .unwrap()
            .grants
            .iter()
            .any(|g| g.tool == "once-gate"));
        submit(&s, &limits, &from, &argv, "fifth".into()).unwrap();
        assert_eq!(count(&marker), 5);
    }

    // These tests require Linux unprivileged user/mount namespaces and mount(8).
    // They deliberately fail if that test environment is unavailable; no skip
    // or alternate generic I/O failure is claimed as ENOSPC evidence.
    fn isolated(test: &str, completion: bool) {
        let test_name = format!("exec::resource_tests::{test}");
        if let Some(root) = std::env::var_os("CLIX_RESOURCE_ENOSPC_CHILD") {
            let root = PathBuf::from(root);
            let state = root.join("mount");
            assert!(Command::new("mount")
                .args(["-t", "tmpfs", "-o", "size=8M", "tmpfs"])
                .arg(&state)
                .status()
                .unwrap()
                .success());
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(enospc_case(&root, &state, completion));
            return;
        }
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("mount")).unwrap();
        let output = Command::new("unshare")
            .args([
                "--user",
                "--map-root-user",
                "--mount",
                "--propagation",
                "private",
            ])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", &test_name, "--nocapture", "--test-threads=1"])
            .env("CLIX_RESOURCE_ENOSPC_CHILD", root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fill(state: &Path) {
        let mut filler = fs::File::create(state.join("filler")).unwrap();
        let block = [0x55; 4096];
        loop {
            match filler.write_all(&block) {
                Ok(()) => {}
                Err(e) => {
                    assert_eq!(e.raw_os_error(), Some(28), "expected actual ENOSPC");
                    break;
                }
            }
        }
    }

    async fn enospc_case(root: &Path, state: &Path, completion: bool) {
        let marker = root.join("marker");
        let gate = root.join("release");
        let mut s = Store::open(state).unwrap();
        s.body_name = "runner".into();
        s.grants.push(grant("gate", fixture(root), true));
        s.save().unwrap();
        let store = Arc::new(Mutex::new(s));
        let limits = crate::limits::Limits::default();
        let argv = vec![
            "gate".into(),
            marker.to_string_lossy().into(),
            gate.to_string_lossy().into(),
        ];
        let from = BodyId("origin".into());
        if completion {
            submit(&store, &limits, &from, &argv, "disk-full".into()).unwrap();
            wait_for(|| count(&marker) == 1).await;
            fill(state);
            fs::write(&gate, []).unwrap();
            wait_for(|| {
                store
                    .lock()
                    .unwrap()
                    .jobs
                    .iter()
                    .any(|j| matches!(j.status, JobStatus::Uncertain { .. }))
            })
            .await;
            assert!(store.lock().unwrap().ensure_writable().is_err());
            assert_eq!(
                store.lock().unwrap().grants[0].reservation.as_deref(),
                Some("disk-full")
            );
        } else {
            fill(state);
            let err = submit(&store, &limits, &from, &argv, "disk-full".into()).unwrap_err();
            assert!(err.to_string().contains("os error 28"), "{err}");
            assert!(!marker.exists());
            let s = store.lock().unwrap();
            assert!(s.jobs.is_empty());
            assert!(s.grants[0].reservation.is_none());
            assert!(s.ensure_writable().is_err());
        }
        fs::remove_file(state.join("filler")).unwrap();
        let mut reopened = Store::open(state).unwrap();
        if completion {
            assert!(matches!(reopened.jobs[0].status, JobStatus::Running));
            reopened.recover_jobs().unwrap();
            assert!(matches!(
                reopened.jobs[0].status,
                JobStatus::Uncertain { .. }
            ));
            assert_eq!(reopened.grants[0].reservation.as_deref(), Some("disk-full"));
            let reopened = Arc::new(Mutex::new(reopened));
            submit(&reopened, &limits, &from, &argv, "disk-full".into()).unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(count(&marker), 1);
        } else {
            assert!(reopened.jobs.is_empty());
            assert!(reopened.grants[0].reservation.is_none());
            assert!(!marker.exists());
        }
    }

    #[test]
    fn enospc_before_admission_never_starts_a_child() {
        isolated("enospc_before_admission_never_starts_a_child", false);
    }

    #[test]
    fn enospc_completion_recovers_uncertain_without_reexecution() {
        isolated(
            "enospc_completion_recovers_uncertain_without_reexecution",
            true,
        );
    }
}
