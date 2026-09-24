//! Real daemon processes: no in-process serve task or direct state mutation.
//! These tests establish localhost process behavior, not two-machine proof.

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{mpsc, Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use clix::{client_send, Grant, Job, JobStatus};
use serde_json::{json, Value};
use tempfile::TempDir;

const DEADLINE: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(50);

fn owner_rpc(sock: &Path, request: Value, deadline: Duration) -> Result<Value, String> {
    let sock = sock.to_path_buf();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(client_send(&sock, request).map_err(|e| e.to_string()));
    });
    rx.recv_timeout(deadline)
        .map_err(|e| format!("owner RPC did not complete within {deadline:?}: {e}"))?
}

fn eventually<T>(description: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(start.elapsed() < DEADLINE, "timed out: {description}");
        thread::sleep(POLL);
    }
}

struct Daemon {
    home: TempDir,
    sock: PathBuf,
    mesh_addr: String,
    child: Option<Child>,
}

impl Daemon {
    fn spawn() -> Self {
        let home = tempfile::tempdir().unwrap();
        let sock = home.path().join("owner.sock");
        let mut daemon = Self {
            home,
            sock,
            mesh_addr: "127.0.0.1:0".into(),
            child: None,
        };
        daemon.start();
        daemon
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clix"));
        command
            .env("CLIX_HOME", self.home.path().join("state"))
            .env("CLIX_SOCK", &self.sock)
            .env("CLIX_PIN", self.home.path().join("src"))
            .env("CLIX_MESH_BIND", &self.mesh_addr)
            .env("CLIX_NOTIFY", "0")
            .env("CLIX_TRAY", "0")
            .env("CLIX_WAIT_POLL", "50ms")
            .env("TOKIO_WORKER_THREADS", "2")
            .env_remove("CLIX_PAIR_ADDR")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY");
        command
    }

    fn start(&mut self) {
        assert!(self.child.is_none());
        let log = File::create(self.home.path().join("daemon.stderr")).unwrap();
        let mut command = self.command();
        // A private process group lets cleanup kill fixture descendants as well.
        command
            .arg("daemon")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        self.child = Some(command.spawn().unwrap());
        let start = Instant::now();
        loop {
            if let Some(exit) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("daemon exited {exit}: {}", self.diagnostics());
            }
            if let Ok(status) = owner_rpc(
                &self.sock,
                json!({"op":"status"}),
                Duration::from_millis(250),
            ) {
                if let Some(addr) = status["mesh_addr"].as_str().filter(|addr| !addr.is_empty()) {
                    self.mesh_addr = addr.into();
                    return;
                }
            }
            assert!(
                start.elapsed() < DEADLINE,
                "daemon did not start: {}",
                self.diagnostics()
            );
            thread::sleep(POLL);
        }
    }

    fn crash(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Kill the actual daemon and its fixture descendants, not a Tokio task.
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn restart(&mut self) {
        self.crash();
        self.start();
    }

    fn diagnostics(&self) -> String {
        fs::read_to_string(self.home.path().join("daemon.stderr")).unwrap_or_default()
    }

    fn rpc(&self, request: Value) -> Value {
        owner_rpc(&self.sock, request.clone(), DEADLINE)
            .unwrap_or_else(|e| panic!("RPC {request}: {e}\n{}", self.diagnostics()))
    }

    fn jobs(&self) -> Vec<Job> {
        serde_json::from_value(self.rpc(json!({"op":"log"}))["jobs"].clone()).unwrap()
    }

    fn hands(&self) -> Vec<Grant> {
        serde_json::from_value(self.rpc(json!({"op":"hands"}))["hands"].clone()).unwrap()
    }

    fn wait_job(&self, id: &str, accept: impl Fn(&JobStatus) -> bool) -> Job {
        eventually(&format!("job {id} to reach expected state"), || {
            self.jobs()
                .into_iter()
                .find(|job| job.id == id && accept(&job.status))
        })
    }

    fn terminal(&self, id: &str) -> Job {
        self.wait_job(id, |status| {
            matches!(
                status,
                JobStatus::Done { .. }
                    | JobStatus::Denied { .. }
                    | JobStatus::Failed { .. }
                    | JobStatus::Uncertain { .. }
            )
        })
    }

    fn fixture(&self, name: &str, body: &str) -> PathBuf {
        let dir = self.home.path().join("tools");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn grant(&self, tool: &Path, once: bool) {
        let response = self.rpc(json!({"op":"add","tool":tool,"once":once}));
        assert_eq!(response["ok"], true, "{response}");
    }

    fn submit(&self, tool: &Path, args: &[&Path]) -> String {
        let response = self.rpc(exec_request(tool, args, true));
        response["job"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{response}"))
            .into()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.cli_in(args, None)
    }

    fn cli_in(&self, args: &[&str], cwd: Option<&Path>) -> Output {
        let capture = tempfile::tempdir().unwrap();
        let stdout = capture.path().join("stdout");
        let stderr = capture.path().join("stderr");
        let mut command = self.command();
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(File::create(&stdout).unwrap())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if start.elapsed() >= DEADLINE {
                let _ = child.kill();
                let _ = child.wait();
                panic!("CLI {args:?} timed out: {}", self.diagnostics());
            }
            thread::sleep(POLL);
        };
        Output {
            status,
            stdout: fs::read(stdout).unwrap(),
            stderr: fs::read(stderr).unwrap(),
        }
    }
}

#[test]
fn owner_cli_selects_the_inspected_request_and_revokes_deleted_relative_paths() {
    let (mut laptop, server) = paired();
    server.rpc(json!({"op":"request", "body":"laptop", "tool":"true"}));
    let inspected = laptop.cli(&["pending"]);
    assert!(inspected.status.success());
    let output = String::from_utf8(inspected.stdout).unwrap();
    let id = output.split_whitespace().next().unwrap();
    let pending = laptop.rpc(json!({"op":"pending"}));
    assert_eq!(id, pending["requests"][0]["id"].as_str().unwrap());
    server.rpc(json!({"op":"request", "body":"laptop", "tool":"false"}));
    assert!(!laptop.cli(&["allow"]).status.success());
    let allowed = laptop.cli(&["allow", id]);
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_eq!(laptop.hands().len(), 1);
    assert_eq!(laptop.hands()[0].tool, "true");
    assert!(laptop.hands()[0].once);
    assert_eq!(
        laptop.hands()[0].allow_from.as_ref().unwrap()[0].0,
        "server"
    );
    laptop.restart();
    assert!(!laptop.cli(&["allow", id]).status.success());
    let b = laptop.rpc(json!({"op":"pending"}))["requests"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(laptop.cli(&["deny", &b]).status.success());
    assert!(laptop.rpc(json!({"op":"pending"}))["requests"]
        .as_array()
        .unwrap()
        .is_empty());
    let tool = laptop.fixture("revocable", "exit 0");
    let cwd = tool.parent().unwrap();
    let added = laptop.cli_in(&["add", "./revocable"], Some(cwd));
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    fs::remove_file(&tool).unwrap();
    let removed = laptop.cli_in(&["remove", "./revocable"], Some(cwd));
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(laptop.hands().iter().all(|g| g.tool != "revocable"));
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.crash();
    }
}

fn paired() -> (Daemon, Daemon) {
    let laptop = Daemon::spawn();
    let server = Daemon::spawn();
    let started = laptop.rpc(json!({"op":"pair_start","name":"laptop"}));
    let phrase = started["phrase"].as_str().unwrap();
    let joined = server.rpc(json!({
        "op":"pair_join","name":"server","phrase":phrase,"addr":laptop.mesh_addr,
    }));
    assert_eq!(joined["ok"], true, "{joined}");
    assert_eq!(laptop.rpc(json!({"op":"pair_await"}))["ok"], true);
    assert_eq!(
        laptop.rpc(json!({"op":"status"}))["peers"][0]["name"],
        "server"
    );
    assert_eq!(
        server.rpc(json!({"op":"status"}))["peers"][0]["name"],
        "laptop"
    );
    (laptop, server)
}

fn tool_name(tool: &Path) -> &str {
    tool.file_name().unwrap().to_str().unwrap()
}

fn exec_request(tool: &Path, args: &[&Path], no_wait: bool) -> Value {
    let mut argv = vec![tool_name(tool).to_string()];
    argv.extend(args.iter().map(|p| p.to_str().unwrap().to_string()));
    json!({"op":"exec","body":"laptop","argv":argv,"no_wait":no_wait})
}

fn gate_fixture(laptop: &Daemon) -> (PathBuf, PathBuf, PathBuf) {
    let tool = laptop.fixture(
        "gated-tool",
        r#"
printf 'run\n' >> "$1"
while [ ! -e "$2" ]; do /bin/sleep 0.05; done
printf 'done\n'
"#,
    );
    (
        tool,
        laptop.home.path().join("runs"),
        laptop.home.path().join("release"),
    )
}

fn run_count(marker: &Path) -> usize {
    fs::read_to_string(marker)
        .unwrap_or_default()
        .lines()
        .count()
}

fn assert_done(job: &Job, exit: i32) {
    assert_eq!(job.status, JobStatus::Done { exit }, "{job:?}");
}

#[test]
fn remote_cli_preserves_binary_stdout_exactly() {
    let (laptop, server) = paired();
    let tool = laptop.fixture("binary-tool", r#"printf '\000\377\376\200A\n'"#);
    laptop.grant(&tool, false);
    let out = server.cli(&["laptop", tool_name(&tool)]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(out.stdout, [0, 255, 254, 128, b'A', b'\n']);
    let origin = server.jobs();
    assert_eq!(origin.len(), 1);
    assert_done(&origin[0], 0);
    assert_eq!(origin[0].stdout, out.stdout);
    let runner = laptop.jobs();
    assert_eq!(runner.len(), 1);
    assert_eq!(runner[0], origin[0]);
}

#[test]
fn remote_cli_preserves_exit_23_and_both_output_streams() {
    let (laptop, server) = paired();
    let tool = laptop.fixture(
        "exit-tool",
        "printf 'before failure\\n'\nprintf 'diagnostic\\n' >&2\nexit 23",
    );
    laptop.grant(&tool, false);
    let out = server.cli(&["laptop", tool_name(&tool)]);
    assert_eq!(out.status.code(), Some(23), "{out:?}");
    assert_eq!(out.stdout, b"before failure\n");
    assert!(out.stderr.ends_with(b"diagnostic\n"), "{out:?}");
    let origin = server.jobs();
    assert_eq!(origin.len(), 1);
    assert_done(&origin[0], 23);
    assert_eq!(origin[0].stderr, b"diagnostic\n");
}

#[test]
fn simultaneous_once_invocations_have_one_execution_and_one_success() {
    let (laptop, server) = paired();
    let (tool, marker, gate) = gate_fixture(&laptop);
    laptop.grant(&tool, true);
    let barrier = Arc::new(Barrier::new(3));
    let mut callers = Vec::new();
    for _ in 0..2 {
        let barrier = barrier.clone();
        let sock = server.sock.clone();
        let request = exec_request(&tool, &[&marker, &gate], true);
        callers.push(thread::spawn(move || {
            barrier.wait();
            owner_rpc(&sock, request, DEADLINE).unwrap()
        }));
    }
    barrier.wait();
    let ids: Vec<String> = callers
        .into_iter()
        .map(|caller| caller.join().unwrap()["job"]["id"].as_str().unwrap().into())
        .collect();
    assert_ne!(ids[0], ids[1]);
    eventually("one running invocation and one denial", || {
        let jobs = laptop.jobs();
        (jobs.len() == 2
            && jobs
                .iter()
                .filter(|j| matches!(j.status, JobStatus::Running))
                .count()
                == 1
            && jobs
                .iter()
                .filter(|j| matches!(j.status, JobStatus::Denied { .. }))
                .count()
                == 1)
            .then_some(())
    });
    eventually("one fixture execution", || {
        (run_count(&marker) == 1).then_some(())
    });
    fs::write(&gate, []).unwrap();
    let results: Vec<Job> = ids.iter().map(|id| server.terminal(id)).collect();
    assert_eq!(
        results
            .iter()
            .filter(|j| j.status == (JobStatus::Done { exit: 0 }))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|j| matches!(j.status, JobStatus::Denied { .. }))
            .count(),
        1
    );
    assert_eq!(run_count(&marker), 1);
    assert!(laptop.hands().is_empty());
}

#[test]
fn running_job_is_visible_on_both_bodies_and_owner_control_is_responsive() {
    let (laptop, server) = paired();
    let (tool, marker, gate) = gate_fixture(&laptop);
    laptop.grant(&tool, false);
    let id = server.submit(&tool, &[&marker, &gate]);
    let runner = laptop.wait_job(&id, |s| matches!(s, JobStatus::Running));
    let origin = server.wait_job(&id, |s| matches!(s, JobStatus::Running));
    assert_eq!(runner, origin);
    let hands = owner_rpc(&laptop.sock, json!({"op":"hands"}), Duration::from_secs(2))
        .expect("owner hands must respond while a tool is running");
    assert_eq!(hands["hands"].as_array().unwrap().len(), 1);
    fs::write(&gate, []).unwrap();
    assert_done(&server.terminal(&id), 0);
}

#[test]
fn queued_job_survives_origin_process_restart_with_the_same_id() {
    let (mut laptop, mut server) = paired();
    let tool = laptop.fixture(
        "count-tool",
        r#"printf 'run\n' >> "$1"; printf 'recovered\n'"#,
    );
    let marker = laptop.home.path().join("runs");
    laptop.grant(&tool, false);
    laptop.crash();
    let id = server.submit(&tool, &[&marker]);
    server.wait_job(&id, |s| matches!(s, JobStatus::WaitingBody));
    assert_eq!(run_count(&marker), 0);
    server.restart();
    let retained = server.wait_job(&id, |s| matches!(s, JobStatus::WaitingBody));
    assert_eq!(retained.id, id);
    laptop.start();
    let completed = server.terminal(&id);
    assert_done(&completed, 0);
    assert_eq!(completed.stdout, b"recovered\n");
    assert_done(&laptop.terminal(&id), 0);
    assert_eq!(server.jobs().len(), 1);
    assert_eq!(laptop.jobs().len(), 1);
    assert_eq!(run_count(&marker), 1);
    // Restarting again must not redeliver a terminal invocation.
    server.restart();
    assert_done(&server.terminal(&id), 0);
    assert_eq!(run_count(&marker), 1);
}

#[test]
fn runner_restart_reports_uncertainty_without_rerunning_or_releasing_once() {
    let (mut laptop, server) = paired();
    let (tool, marker, gate) = gate_fixture(&laptop);
    laptop.grant(&tool, true);
    let id = server.submit(&tool, &[&marker, &gate]);
    laptop.wait_job(&id, |s| matches!(s, JobStatus::Running));
    server.wait_job(&id, |s| matches!(s, JobStatus::Running));
    eventually("fixture took effect before runner crash", || {
        (run_count(&marker) == 1).then_some(())
    });
    laptop.restart();
    let runner = laptop.wait_job(&id, |s| matches!(s, JobStatus::Uncertain { .. }));
    let origin = server.terminal(&id);
    assert!(
        matches!(origin.status, JobStatus::Uncertain { .. }),
        "{origin:?}"
    );
    assert_eq!(runner, origin);
    let grants = laptop.hands();
    assert_eq!(grants.len(), 1);
    assert!(grants[0].once);
    assert_eq!(grants[0].reservation.as_deref(), Some(id.as_str()));
    fs::write(&gate, []).unwrap();
    let inspection = server.cli(&["job", "inspect", &id]);
    assert!(inspection.status.success());
    assert!(String::from_utf8_lossy(&inspection.stdout).contains("uncertain"));
    let attempt = server.cli(&["job", "retry", &id]);
    assert!(attempt.status.success(), "{attempt:?}");
    let retry = String::from_utf8(attempt.stdout).unwrap().trim().to_owned();
    assert_ne!(retry, id);
    assert_eq!(
        server.rpc(json!({"op":"job_inspect","job_id":retry}))["view"]["retry_of"],
        id
    );
    let denied = server.terminal(&retry);
    assert!(
        matches!(denied.status, JobStatus::Denied { .. }),
        "{denied:?}"
    );
    assert_eq!(run_count(&marker), 1);
    assert_eq!(laptop.hands()[0].reservation.as_deref(), Some(id.as_str()));
}

#[test]
fn owner_cli_retrieves_exact_saved_output_and_prunes_without_erasing_outcomes() {
    let (laptop, server) = paired();
    let tool = laptop.fixture(
        "saved-bytes",
        r#"printf '\000\377A\n'; printf 'error\n' >&2"#,
    );
    laptop.grant(&tool, false);
    let out = server.cli(&["laptop", tool_name(&tool)]);
    assert!(out.status.success(), "{out:?}");
    let id = server.jobs()[0].id.clone();
    let saved = server.cli(&["job", "output", &id]);
    assert!(saved.status.success(), "{saved:?}");
    assert_eq!(saved.stdout, out.stdout);
    let stderr = server.cli(&["job", "output", &id, "--stderr"]);
    assert!(stderr.status.success());
    assert_eq!(stderr.stdout, b"error\n");
    assert!(server
        .cli(&["storage", "prune", "--keep-output-jobs", "1"])
        .status
        .success());
    assert_eq!(server.cli(&["job", "output", &id]).stdout, out.stdout);
    assert!(server.cli(&["storage", "prune"]).status.success());
    assert!(!server.cli(&["job", "output", &id]).status.success());
    let view = server.rpc(json!({"op":"job_inspect","job_id":id}));
    assert_eq!(view["view"]["output_available"], false);
    assert_eq!(view["view"]["stdout"]["bytes"], out.stdout.len());
    assert_done(&server.terminal(&id), 0);
    // The runner still retains bytes, but subsequent replication must not
    // undo the owner's local output-retention decision.
    eventually("history synchronization after prune", || {
        let status = server.rpc(json!({"op":"status"}));
        (status["history"]["peers"][0]["complete"] == true).then_some(())
    });
    assert!(!server.cli(&["job", "output", &id]).status.success());
}

fn cli_json(daemon: &Daemon, args: &[&str]) -> Value {
    let output = daemon.cli(args);
    assert!(output.status.success(), "{args:?}: {output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn owner_command_names_do_not_make_paired_bodies_unusable_after_restart() {
    let mut target = Daemon::spawn();
    let mut origin = Daemon::spawn();
    let invitation = target.rpc(json!({"op":"pair_start","name":"pin"}));
    origin.rpc(json!({"op":"pair_join","name":"job","phrase":invitation["phrase"],"addr":target.mesh_addr}));
    target.rpc(json!({"op":"pair_await"}));
    target.rpc(json!({"op":"add","tool":"true","allow":["job"]}));
    target.restart();
    origin.restart();
    let output = origin.cli(&["--", "pin", "true"]);
    assert!(output.status.success(), "{output:?}");
    let jobs = origin.jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].from.0, "job");
    assert_eq!(jobs[0].body.0, "pin");
    assert_done(&jobs[0], 0);
    assert_eq!(
        cli_json(&origin, &["pin", "recovery", "list"])["body"],
        "job"
    );
}

#[test]
fn owner_cli_reviews_restores_exports_and_discards_retained_pin_versions() {
    let (laptop, server) = paired();
    let live = laptop.home.path().join("src/note");
    let peer = server.home.path().join("src/note");
    fs::write(&live, b"original\0bytes").unwrap();
    cli_json(&laptop, &["pin", "sync", "server"]);
    fs::write(&peer, b"peer edit").unwrap();
    cli_json(&laptop, &["pin", "sync", "server"]);
    assert_eq!(fs::read(&live).unwrap(), b"peer edit");
    let list = cli_json(&laptop, &["pin", "recovery", "list"]);
    let id = list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["state"] == "retained")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let inspected = cli_json(&laptop, &["pin", "recovery", "inspect", id]);
    let token = inspected["token"].as_str().unwrap();
    let exported = laptop.cli(&[
        "pin", "recovery", "export", id, "retained", "--token", token,
    ]);
    assert!(exported.status.success(), "{exported:?}");
    assert_eq!(exported.stdout, b"original\0bytes");
    fs::write(&live, b"new local edit").unwrap();
    assert!(!laptop
        .cli(&[
            "pin",
            "recovery",
            "resolve",
            id,
            "restore-retained",
            "--token",
            token
        ])
        .status
        .success());
    assert_eq!(fs::read(&live).unwrap(), b"new local edit");
    let current = cli_json(&laptop, &["pin", "recovery", "inspect", id]);
    let resolved = cli_json(
        &laptop,
        &[
            "pin",
            "recovery",
            "resolve",
            id,
            "restore-retained",
            "--token",
            current["token"].as_str().unwrap(),
        ],
    );
    assert_eq!(fs::read(&live).unwrap(), b"original\0bytes");
    let list = cli_json(&laptop, &["pin", "recovery", "list"]);
    assert_eq!(
        list["entries"].as_array().unwrap().len(),
        2,
        "displaced live version must be retained: {list}"
    );
    cli_json(
        &laptop,
        &[
            "pin",
            "recovery",
            "discard",
            id,
            "--token",
            resolved["token"].as_str().unwrap(),
        ],
    );
    assert_eq!(fs::read(&live).unwrap(), b"original\0bytes");
    assert!(!laptop
        .cli(&["pin", "recovery", "inspect", id])
        .status
        .success());
    assert!(laptop.hands().is_empty());
    assert!(laptop.jobs().is_empty());
}

#[test]
fn owner_cli_takes_only_the_fresh_peer_conflict_without_running_a_tool() {
    let (laptop, server) = paired();
    let live = laptop.home.path().join("src/conflict");
    let peer = server.home.path().join("src/conflict");
    fs::write(&live, b"baseline").unwrap();
    cli_json(&laptop, &["pin", "sync", "server"]);
    fs::write(&live, b"local").unwrap();
    fs::write(&peer, b"peer").unwrap();
    assert!(!laptop.cli(&["pin", "sync", "server"]).status.success());
    let report = cli_json(&laptop, &["pin", "conflicts", "server"]);
    let token = report["conflicts"][0]["token"].as_str().unwrap();
    fs::write(&peer, b"new peer").unwrap();
    assert!(!laptop
        .cli(&["pin", "take-peer", "server", "conflict", "--token", token])
        .status
        .success());
    assert_eq!(fs::read(&live).unwrap(), b"local");
    let report = cli_json(&laptop, &["pin", "conflicts", "server"]);
    cli_json(
        &laptop,
        &[
            "pin",
            "take-peer",
            "server",
            "conflict",
            "--token",
            report["conflicts"][0]["token"].as_str().unwrap(),
        ],
    );
    assert_eq!(fs::read(&live).unwrap(), b"new peer");
    cli_json(&laptop, &["pin", "sync", "server"]);
    assert!(laptop.hands().is_empty());
    assert!(server.hands().is_empty());
    assert!(laptop.jobs().is_empty());
    assert!(server.jobs().is_empty());
}

#[test]
fn shared_history_survives_process_restarts_and_relay_with_original_runner_offline() {
    let (mut laptop, mut server) = paired();
    let mut third = Daemon::spawn();
    for (daemon, name) in [(&laptop, "laptop"), (&server, "server")] {
        let invitation = daemon.rpc(json!({"op":"pair_start","name":name}));
        third.rpc(json!({"op":"pair_join","name":"third","phrase":invitation["phrase"],"addr":daemon.mesh_addr}));
        daemon.rpc(json!({"op":"pair_await"}));
    }
    third.crash();
    let tool = laptop.fixture("local-result", "printf 'local result\\n'");
    laptop.grant(&tool, false);
    let id = laptop.submit(&tool, &[]);
    assert_done(&laptop.terminal(&id), 0);
    assert_eq!(server.terminal(&id).stdout, b"local result\n");
    laptop.crash();
    server.restart();
    third.start();
    assert_eq!(third.terminal(&id).stdout, b"local result\n");
    third.restart();
    let view = third.rpc(json!({"op":"job_inspect","job_id":id}));
    assert_eq!(view["view"]["provenance"], "verified runner result");
    let state: Value =
        serde_json::from_slice(&fs::read(third.home.path().join("state/state.json")).unwrap())
            .unwrap();
    assert!(
        state["jobs"].as_array().unwrap().is_empty(),
        "imported records must never enter local admission"
    );
    assert!(third.hands().is_empty());
    assert!(!third.cli(&["job", "retry", &id]).status.success());
}

#[test]
fn finishing_an_old_run_does_not_consume_the_owners_replacement_once_grant() {
    let (laptop, server) = paired();
    let (tool, marker, gate) = gate_fixture(&laptop);
    laptop.grant(&tool, true);
    let old_id = server.submit(&tool, &[&marker, &gate]);
    laptop.wait_job(&old_id, |s| matches!(s, JobStatus::Running));
    eventually("first invocation started", || {
        (run_count(&marker) == 1).then_some(())
    });
    laptop.grant(&tool, true);
    assert!(laptop.hands()[0].reservation.is_none());
    fs::write(&gate, []).unwrap();
    assert_done(&server.terminal(&old_id), 0);
    let replacement = laptop.hands();
    assert_eq!(replacement.len(), 1);
    assert!(replacement[0].once);
    assert!(replacement[0].reservation.is_none());
    let new_id = server.submit(&tool, &[&marker, &gate]);
    assert_done(&server.terminal(&new_id), 0);
    assert_eq!(run_count(&marker), 2);
    assert!(laptop.hands().is_empty());
}

#[test]
fn output_over_four_mib_per_stream_returns_bounded_failure() {
    let (laptop, server) = paired();
    for (name, script) in [
        (
            "large-stdout",
            "/bin/dd if=/dev/zero bs=1048576 count=5 2>/dev/null",
        ),
        (
            "large-stderr",
            "/bin/dd if=/dev/zero bs=1048576 count=5 3>&2 2>/dev/null 1>&3",
        ),
    ] {
        let tool = laptop.fixture(name, script);
        laptop.grant(&tool, false);
        let id = server.submit(&tool, &[]);
        let completed = server.terminal(&id);
        let reason = match &completed.status {
            JobStatus::Failed { reason } => reason,
            other => panic!("oversized {name} must be an explicit failure, got {other:?}"),
        };
        assert!(reason.to_ascii_lowercase().contains("output"), "{reason}");
        assert!(completed.stdout.len() <= 4 * 1024 * 1024);
        assert!(completed.stderr.len() <= 4 * 1024 * 1024);
        assert_eq!(laptop.terminal(&id), completed);
        assert_eq!(
            laptop.rpc(json!({"op":"hands"}))["hands"]
                .as_array()
                .unwrap()
                .len(),
            if name == "large-stdout" { 1 } else { 2 }
        );
    }
}

#[test]
fn failed_once_run_releases_the_reservation_and_later_success_consumes_it() {
    let (laptop, server) = paired();
    let tool = laptop.fixture(
        "fail-then-succeed",
        r#"
printf 'run\n' >> "$1"
if [ ! -e "$2" ]; then exit 23; fi
printf 'success\n'
"#,
    );
    let marker = laptop.home.path().join("runs");
    let gate = laptop.home.path().join("succeed");
    laptop.grant(&tool, true);
    let failed_id = server.submit(&tool, &[&marker, &gate]);
    assert_done(&server.terminal(&failed_id), 23);
    let grants = laptop.hands();
    assert_eq!(grants.len(), 1);
    assert!(grants[0].once);
    assert!(grants[0].reservation.is_none());
    fs::write(&gate, []).unwrap();
    let succeeded_id = server.submit(&tool, &[&marker, &gate]);
    assert_done(&server.terminal(&succeeded_id), 0);
    assert_eq!(run_count(&marker), 2);
    assert!(laptop.hands().is_empty());
}
#[test]
fn successful_once_run_is_consumed_when_descendant_output_exceeds_the_limit() {
    let (laptop, server) = paired();
    let tool = laptop.fixture(
        "successful-parent-large-output",
        r#"
printf 'run\n' >> "$1"
printf '%s\n' "$$" > "$2"
(
    while [ ! -e "$3" ]; do /bin/sleep 0.05; done
    /bin/dd if=/dev/zero bs=1048576 count=5 2>/dev/null
) &
exit 0
"#,
    );
    let marker = laptop.home.path().join("successful-runs");
    let pid_file = laptop.home.path().join("parent-pid");
    let output_gate = laptop.home.path().join("release-output");
    laptop.grant(&tool, true);

    let id = server.submit(&tool, &[&marker, &pid_file, &output_gate]);
    // The primary granted process must actually finish before the descendant
    // produces excessive output. Linux reports a terminated, unreaped child
    // as Z; an already-reaped child has no /proc entry.
    eventually("successful primary process has exited", || {
        let pid: u32 = fs::read_to_string(&pid_file).ok()?.trim().parse().ok()?;
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => {
                let after_name = stat.rsplit_once(')')?.1;
                (after_name.split_whitespace().next() == Some("Z")).then_some(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(()),
            Err(e) => panic!("could not inspect fixture process {pid}: {e}"),
        }
    });
    assert_eq!(run_count(&marker), 1);
    fs::write(&output_gate, []).unwrap();

    let completed = server.terminal(&id);
    match &completed.status {
        JobStatus::Failed { reason } => assert!(reason.contains("output"), "{reason}"),
        other => panic!("output limit must be an explicit failure, got {other:?}"),
    }
    assert!(
        laptop.hands().is_empty(),
        "the primary tool exited 0, so its once grant must be consumed despite capture failure"
    );

    let retry = server.submit(&tool, &[&marker, &pid_file, &output_gate]);
    let denied = server.terminal(&retry);
    assert!(
        matches!(denied.status, JobStatus::Denied { .. }),
        "{denied:?}"
    );
    assert_eq!(
        run_count(&marker),
        1,
        "the successful once invocation must not run twice"
    );
}

#[test]
fn offline_permission_request_survives_origin_restart_without_running_a_tool() {
    let (mut laptop, mut server) = paired();
    laptop.crash();
    let request = server.rpc(json!({"op":"request", "body":"laptop", "tool":"true"}));
    assert_eq!(request["status"], "waiting");
    let id = request["request_id"].as_str().unwrap();
    server.restart();
    assert!(server.rpc(json!({"op":"status"}))["outbound_requests"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["id"] == id));
    laptop.start();
    eventually("request delivered after both sidecars restart", || {
        let pending = laptop.rpc(json!({"op":"pending"}));
        (pending["requests"].as_array()?.len() == 1).then_some(())
    });
    eventually("origin records delivery", || {
        server.rpc(json!({"op":"status"}))["outbound_requests"]
            .as_array()?
            .is_empty()
            .then_some(())
    });
    assert!(laptop.jobs().is_empty());
    assert!(server.jobs().is_empty());
}

// Linux acceptance prerequisite: unprivileged user/mount/PID namespaces and mount(8).
// These tests fail if that environment is absent; no skip substitutes another I/O error.

fn isolated_process_enospc(name: &str, run: impl FnOnce()) {
    if std::env::var("CLIX_PROCESS_ENOSPC_CHILD").as_deref() == Ok(name) {
        run();
        return;
    }
    let logs = tempfile::tempdir().unwrap();
    let stdout = logs.path().join("stdout");
    let stderr = logs.path().join("stderr");
    let mut child = Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--pid",
            "--fork",
            "--kill-child",
            "--propagation",
            "private",
        ])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env("CLIX_PROCESS_ENOSPC_CHILD", name)
        .stdin(Stdio::null())
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated ENOSPC process test timed out");
        }
        thread::sleep(POLL);
    };
    assert!(
        status.success(),
        "isolated {name} failed:\n{}\n{}",
        fs::read_to_string(stdout).unwrap(),
        fs::read_to_string(stderr).unwrap()
    );
}

fn daemon_with_private_tmpfs_state() -> Daemon {
    let home = tempfile::tempdir().unwrap();
    let state = home.path().join("state");
    fs::create_dir(&state).unwrap();
    assert!(Command::new("mount")
        .args(["-t", "tmpfs", "-o", "size=8M", "tmpfs"])
        .arg(&state)
        .status()
        .unwrap()
        .success());
    let sock = home.path().join("owner.sock");
    let mut daemon = Daemon {
        home,
        sock,
        mesh_addr: "127.0.0.1:0".into(),
        child: None,
    };
    daemon.start();
    // Give the disposable local body a stable name through an owner operation.
    // No second body or external account is trusted by this fixture.
    daemon.rpc(json!({"op":"pair_start", "name":"laptop"}));
    daemon
}

fn exhaust_private_state_mount(daemon: &Daemon) -> PathBuf {
    use std::io::Write;
    let filler = daemon.home.path().join("state/filler");
    let mut file = File::create(&filler).unwrap();
    loop {
        match file.write_all(&[0x55; 4096]) {
            Ok(()) => {}
            Err(e) => {
                assert_eq!(
                    e.raw_os_error(),
                    Some(28),
                    "fixture must produce actual ENOSPC"
                );
                break;
            }
        }
    }
    filler
}

fn start_and_wait_for_owner_only(daemon: &mut Daemon) {
    assert!(daemon.child.is_none());
    let mut command = daemon.command();
    command
        .arg("daemon")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(daemon.home.path().join("daemon.stderr")).unwrap());
    daemon.child = Some(command.spawn().unwrap());
    eventually("owner listener despite unsaved restart recovery", || {
        if let Some(exit) = daemon.child.as_mut().unwrap().try_wait().unwrap() {
            panic!(
                "sidecar exited {exit} instead of serving owner inspection: {}",
                daemon.diagnostics()
            );
        }
        owner_rpc(
            &daemon.sock,
            json!({"op":"status"}),
            Duration::from_millis(250),
        )
        .ok()
    });
}

fn assert_unsaved_uncertainty(daemon: &Daemon, id: &str) {
    let inspected = daemon.rpc(json!({"op":"job_inspect", "job_id":id}));
    assert!(
        inspected["view"]["job"]["status"]["Uncertain"].is_object(),
        "{inspected}"
    );
    assert!(
        inspected["view"]["local_warning"]
            .as_str()
            .is_some_and(|s| s.contains("has not been saved")),
        "{inspected}"
    );
    assert!(
        inspected["view"]["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["role"] == "runner" && o["status"] == "Running"),
        "last signed Running evidence must remain distinguishable: {inspected}"
    );
    let cli = daemon.cli(&["job", "inspect", id]);
    assert!(cli.status.success());
    assert!(String::from_utf8_lossy(&cli.stdout)
        .to_ascii_lowercase()
        .contains("uncertain"));
    assert!(String::from_utf8_lossy(&cli.stderr).contains("has not been saved"));
    let status = daemon.rpc(json!({"op":"storage_status"}));
    assert!(status["storage_error"].is_string(), "{status}");
    assert_eq!(daemon.hands()[0].reservation.as_deref(), Some(id));
    let durable: Value =
        serde_json::from_slice(&fs::read(daemon.home.path().join("state/state.json")).unwrap())
            .unwrap();
    assert!(durable["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|j| j["id"] == id && j["status"] == "Running"));
}

#[test]
fn enospc_completion_exposes_unsaved_uncertainty_without_rewriting_signed_evidence() {
    isolated_process_enospc(
        "enospc_completion_exposes_unsaved_uncertainty_without_rewriting_signed_evidence",
        || {
            let mut daemon = daemon_with_private_tmpfs_state();
            let (tool, marker, gate) = gate_fixture(&daemon);
            daemon.grant(&tool, true);
            let id = daemon.submit(&tool, &[&marker, &gate]);
            eventually("fixture took effect", || {
                (run_count(&marker) == 1).then_some(())
            });
            let filler = exhaust_private_state_mount(&daemon);
            fs::write(&gate, []).unwrap();
            eventually("completion save failed with full isolated mount", || {
                let storage = daemon.rpc(json!({"op":"storage_status"}));
                storage["storage_error"].is_string().then_some(())
            });
            assert_unsaved_uncertainty(&daemon, &id);
            let retry = owner_rpc(
                &daemon.sock,
                json!({"op":"job_retry","job_id":id}),
                DEADLINE,
            );
            assert!(retry.is_err(), "poisoned storage admitted a new attempt");
            assert_eq!(run_count(&marker), 1);
            fs::remove_file(filler).unwrap();
            daemon.restart();
            assert!(matches!(
                daemon.terminal(&id).status,
                JobStatus::Uncertain { .. }
            ));
            assert_eq!(daemon.hands()[0].reservation.as_deref(), Some(id.as_str()));
            assert_eq!(run_count(&marker), 1);
        },
    );
}

#[test]
fn enospc_restart_keeps_owner_inspection_available_and_execution_disabled() {
    isolated_process_enospc(
        "enospc_restart_keeps_owner_inspection_available_and_execution_disabled",
        || {
            let mut daemon = daemon_with_private_tmpfs_state();
            let (tool, marker, gate) = gate_fixture(&daemon);
            daemon.grant(&tool, true);
            let id = daemon.submit(&tool, &[&marker, &gate]);
            eventually("fixture took effect", || {
                (run_count(&marker) == 1).then_some(())
            });
            let filler = exhaust_private_state_mount(&daemon);
            daemon.crash();
            start_and_wait_for_owner_only(&mut daemon);
            assert_unsaved_uncertainty(&daemon, &id);
            assert_eq!(daemon.rpc(json!({"op":"status"}))["mesh_addr"], "");
            let attempt = owner_rpc(
                &daemon.sock,
                exec_request(&tool, &[&marker, &gate], true),
                DEADLINE,
            );
            assert!(attempt.is_err(), "recovery failure admitted execution");
            assert_eq!(run_count(&marker), 1);
            fs::remove_file(filler).unwrap();
            daemon.restart();
            assert!(matches!(
                daemon.terminal(&id).status,
                JobStatus::Uncertain { .. }
            ));
            assert_eq!(daemon.hands()[0].reservation.as_deref(), Some(id.as_str()));
            assert_eq!(run_count(&marker), 1);
        },
    );
}

#[test]
fn hands_cli_shows_grant_scope_not_just_the_tool_name() {
    let (laptop, server) = paired();
    let tool = laptop.fixture("scoped-tool", "true");
    // Grant to the server only, single-use.
    let response = laptop.rpc(json!({"op":"add","tool":&tool,"allow":["server"],"once":true}));
    assert_eq!(response["ok"], true, "{response}");
    let out = laptop.cli(&["hands"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(tool_name(&tool)), "hands output: {text}");
    assert!(text.contains("server"), "scope hidden: {text}");
    assert!(text.contains("once"), "single-use hidden: {text}");
    let _ = server;
}
