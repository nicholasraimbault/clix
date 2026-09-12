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
    from: &BodyId,
    argv: &[String],
    id: String,
) -> Result<Value> {
    if argv.is_empty() || argv[0].is_empty() {
        return Err(ClixError::Usage("usage: clix <body> <cmd>…".into()));
    }
    let (accepted, grant) = {
        let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = s.jobs.iter().find(|j| j.id == id) {
            if existing.from != *from || existing.body.0 != s.body_name || existing.argv != argv {
                return Err(ClixError::Protocol(
                    "job ID belongs to a different invocation".into(),
                ));
            }
            return Ok(job::exec_json(existing));
        }
        s.update(|s| {
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
                    if g.once {
                        s.grants
                            .iter_mut()
                            .find(|x| x.tool == g.tool)
                            .unwrap()
                            .reservation = Some(id.clone());
                    }
                    Some(g)
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
            s.jobs.push(j.clone());
            Ok((j, granted))
        })?
    };
    if let Some(grant) = grant {
        let store = store.clone();
        let run = accepted.clone();
        tokio::spawn(async move {
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

const OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;
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
