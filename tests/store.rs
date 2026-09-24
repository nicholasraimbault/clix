use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use chrono::{Local, NaiveTime, TimeZone, Weekday};
use clix::{socket_path, state_dir};
use clix::{BodyId, Grant, Job, JobStatus, Peer, Request, Schedule, Store};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvRestore {
    key: &'static str,
    old: Option<OsString>,
}

impl EnvRestore {
    fn set(key: &'static str, val: impl AsRef<OsStr>) -> Self {
        let old = std::env::var_os(key);
        std::env::set_var(key, val);
        Self { key, old }
    }

    fn unset(key: &'static str) -> Self {
        let old = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, old }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn grant_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    s.grants.push(Grant {
        reservation: None,
        args: None,
        tool: "adb".into(),
        binary: PathBuf::from("/usr/bin/adb"),
        allow_from: None,
        once: false,
        until: None,
        schedule: None,
    });
    s.save().unwrap();
    assert!(dir.path().join("state.json").is_file());
    let s2 = Store::open(dir.path()).unwrap();
    assert_eq!(s2.grants[0].tool, "adb");
    assert!(s2.grants[0].allow_from.is_none());
    assert!(s2.grants[0].schedule.is_none());
}

#[test]
fn grant_schedule_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    s.grants.push(Grant {
        reservation: None,
        args: None,
        tool: "adb".into(),
        binary: PathBuf::from("/usr/bin/adb"),
        allow_from: Some(vec![BodyId("server".into())]),
        once: false,
        until: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000)),
        schedule: Some(Schedule {
            days: vec![Weekday::Mon, Weekday::Wed, Weekday::Fri],
            dates: vec![1, 15],
            from: Some(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
            to: Some(NaiveTime::from_hms_opt(17, 0, 0).unwrap()),
        }),
    });
    s.save().unwrap();
    let s2 = Store::open(dir.path()).unwrap();
    let g = &s2.grants[0];
    assert_eq!(g.tool, "adb");
    assert_eq!(g.binary, PathBuf::from("/usr/bin/adb"));
    assert_eq!(
        g.allow_from.as_ref().unwrap(),
        &vec![BodyId("server".into())]
    );
    assert!(!g.once);
    assert!(g.until.is_some());
    let sch = g.schedule.as_ref().expect("schedule");
    assert_eq!(sch.days, vec![Weekday::Mon, Weekday::Wed, Weekday::Fri]);
    assert_eq!(sch.dates, vec![1, 15]);
    assert_eq!(sch.from, NaiveTime::from_hms_opt(9, 0, 0));
    assert_eq!(sch.to, NaiveTime::from_hms_opt(17, 0, 0));
}

#[test]
fn empty_grants_persist_empty() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    assert!(s.grants.is_empty());
    s.save().unwrap();
    let s2 = Store::open(dir.path()).unwrap();
    assert!(s2.grants.is_empty(), "empty grants is not allow-all");
}

#[test]
fn job_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    s.jobs.push(Job {
        stdout: Vec::new(),
        stderr: Vec::new(),
        id: "job-1".into(),
        body: BodyId("laptop".into()),
        argv: vec!["adb".into(), "devices".into()],
        from: BodyId("server".into()),
        status: JobStatus::WaitingBody,
    });
    s.jobs.push(Job {
        stdout: Vec::new(),
        stderr: Vec::new(),
        id: "job-2".into(),
        body: BodyId("laptop".into()),
        argv: vec!["true".into()],
        from: BodyId("server".into()),
        status: JobStatus::Done { exit: 0 },
    });
    s.jobs.push(Job {
        stdout: Vec::new(),
        stderr: Vec::new(),
        id: "job-3".into(),
        body: BodyId("laptop".into()),
        argv: vec!["bash".into()],
        from: BodyId("server".into()),
        status: JobStatus::Denied {
            reason: "bash is not added on laptop".into(),
        },
    });
    s.jobs.push(Job {
        stdout: Vec::new(),
        stderr: Vec::new(),
        id: "job-4".into(),
        body: BodyId("laptop".into()),
        argv: vec!["adb".into()],
        from: BodyId("server".into()),
        status: JobStatus::Running,
    });
    s.jobs.push(Job {
        stdout: Vec::new(),
        stderr: Vec::new(),
        id: "job-5".into(),
        body: BodyId("laptop".into()),
        argv: vec!["adb".into()],
        from: BodyId("server".into()),
        status: JobStatus::Failed {
            reason: "spawn failed".into(),
        },
    });
    s.save().unwrap();
    let s2 = Store::open(dir.path()).unwrap();
    assert_eq!(s2.jobs.len(), 5);
    assert_eq!(s2.jobs[0].status, JobStatus::WaitingBody);
    assert_eq!(s2.jobs[1].status, JobStatus::Done { exit: 0 });
    match &s2.jobs[2].status {
        JobStatus::Denied { reason } => assert!(reason.contains("not added")),
        other => panic!("expected denied, got {other:?}"),
    }
    assert_eq!(s2.jobs[3].status, JobStatus::Running);
    match &s2.jobs[4].status {
        JobStatus::Failed { reason } => assert_eq!(reason, "spawn failed"),
        other => panic!("expected failed, got {other:?}"),
    }
}

#[test]
fn identity_peers_requests_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    s.body_name = "laptop".into();
    s.peers.push(Peer {
        name: BodyId("server".into()),
        owner_pk: vec![1, 2, 3],
        addr: Some("127.0.0.1:1".into()),
    });
    s.requests.push(Request {
        id: "req-1".into(),
        from: BodyId("server".into()),
        tool: "adb".into(),
        once_suggested: true,
    });
    s.save().unwrap();
    let s2 = Store::open(dir.path()).unwrap();
    assert_eq!(s2.body_name, "laptop");
    assert_eq!(s2.peers[0].name, BodyId("server".into()));
    assert_eq!(s2.peers[0].addr.as_deref(), Some("127.0.0.1:1"));
    assert_eq!(s2.requests[0].tool, "adb");
    assert!(s2.requests[0].once_suggested);
}

#[test]
fn owner_sk_generated_once() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    assert!(!s.owner_sk.is_empty());
    let first = s.owner_sk.clone();
    s.save().unwrap();
    let s2 = Store::open(dir.path()).unwrap();
    assert_eq!(s2.owner_sk, first);
}

#[test]
fn state_dir_uses_clix_home() {
    let _lock = env_lock();
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set("CLIX_HOME", dir.path());
    let got = state_dir().unwrap();
    assert_eq!(got, dir.path());
}

#[test]
fn store_under_clix_home_writes_state_json() {
    let _lock = env_lock();
    let dir = tempfile::tempdir().unwrap();
    let _home = EnvRestore::set("CLIX_HOME", dir.path());
    let mut s = Store::open(&state_dir().unwrap()).unwrap();
    s.grants.push(Grant {
        reservation: None,
        args: None,
        tool: "adb".into(),
        binary: PathBuf::from("/usr/bin/adb"),
        allow_from: None,
        once: false,
        until: None,
        schedule: None,
    });
    s.save().unwrap();
    assert!(dir.path().join("state.json").is_file());
}

#[test]
fn state_dir_default_is_project_dirs() {
    let _lock = env_lock();
    let _home = EnvRestore::unset("CLIX_HOME");
    let got = state_dir().unwrap();
    assert!(
        got.ends_with("clix"),
        "ProjectDirs application is clix, got {got:?}"
    );
}

#[test]
fn socket_path_prefers_clix_sock() {
    let _lock = env_lock();
    let _sock = EnvRestore::set("CLIX_SOCK", "/tmp/clix-test.sock");
    let got = socket_path().unwrap();
    assert_eq!(got, PathBuf::from("/tmp/clix-test.sock"));
}

#[test]
fn socket_path_uses_xdg_runtime_dir() {
    let _lock = env_lock();
    let _sock = EnvRestore::unset("CLIX_SOCK");
    let _xdg = EnvRestore::set("XDG_RUNTIME_DIR", "/run/user/1000");
    let got = socket_path().unwrap();
    assert_eq!(got, PathBuf::from("/run/user/1000/clix.sock"));
}

#[test]
fn schedule_days_or_dates_union() {
    let s = Schedule {
        days: vec![Weekday::Fri],
        dates: vec![1],
        from: None,
        to: None,
    };
    assert!(s.allows_at(local_dt(2026, 9, 4, 12, 0))); // Friday
    assert!(s.allows_at(local_dt(2026, 9, 1, 12, 0))); // 1st, Tuesday
    assert!(!s.allows_at(local_dt(2026, 9, 2, 12, 0))); // Tue 2nd
}

#[test]
fn schedule_hours_use_local_clock() {
    let s = Schedule {
        days: vec![],
        dates: vec![],
        from: Some(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
        to: Some(NaiveTime::from_hms_opt(17, 0, 0).unwrap()),
    };
    assert!(s.allows_at(local_dt(2026, 9, 11, 9, 0)));
    assert!(s.allows_at(local_dt(2026, 9, 11, 17, 0)));
    assert!(!s.allows_at(local_dt(2026, 9, 11, 8, 59)));
    assert!(!s.allows_at(local_dt(2026, 9, 11, 17, 1)));
}

#[test]
fn schedule_empty_calendar_is_every_day() {
    let s = Schedule {
        days: vec![],
        dates: vec![],
        from: None,
        to: None,
    };
    assert!(s.allows_at(local_dt(2026, 9, 12, 0, 0))); // Saturday
}

fn local_dt(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::DateTime<Local> {
    let naive = chrono::NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(h, min, 0)
        .unwrap();
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t,
        chrono::LocalResult::None => panic!("no local time for {naive}"),
    }
}

#[test]
fn secrets_are_private_and_failed_persistence_stops_further_mutations() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(path.join("state.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let original = store.body_name.clone();
    std::fs::rename(&path, dir.path().join("saved-state")).unwrap();
    std::fs::write(&path, "blocked").unwrap();
    assert!(store
        .update(|s| {
            s.body_name = "changed".into();
            Ok(())
        })
        .is_err());
    assert_eq!(store.body_name, original);
    std::fs::remove_file(&path).unwrap();
    assert!(store
        .update(|s| {
            s.body_name = "changed".into();
            Ok(())
        })
        .is_err());
}

#[test]
fn saved_state_carries_a_format_version_older_binaries_refuse() {
    // Grants can now carry an argument allowlist; a binary that predates it
    // would silently drop the list and run the tool unconstrained. Saving must
    // stamp a format version such binaries refuse, and this binary must refuse
    // a state from a still-newer one rather than misread it.
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    s.update(|_| Ok(())).unwrap();
    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(raw["format_version"], 2, "{raw}");

    // A state written by the previous binary (version 1) still opens here.
    let mut v1 = raw.clone();
    v1["format_version"] = serde_json::json!(1);
    std::fs::write(
        dir.path().join("state.json"),
        serde_json::to_vec(&v1).unwrap(),
    )
    .unwrap();
    Store::open(dir.path()).expect("version 1 state must still open");

    // A state from a newer binary is refused, preserved, not misread.
    let mut v3 = raw;
    v3["format_version"] = serde_json::json!(3);
    std::fs::write(
        dir.path().join("state.json"),
        serde_json::to_vec(&v3).unwrap(),
    )
    .unwrap();
    let err = Store::open(dir.path()).unwrap_err().to_string();
    assert!(err.contains("newer Clix"), "{err}");
}
