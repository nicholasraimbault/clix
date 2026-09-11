use std::path::PathBuf;

use clix::{add, check, consume_once, run_granted, BodyId, Grant, Store};

fn empty_store() -> Store {
    let dir = tempfile::tempdir().unwrap();
    Store::open(dir.path()).unwrap()
}

#[test]
fn runs_binary_not_shell() {
    let g = Grant {
        tool: "true".into(),
        binary: PathBuf::from("/usr/bin/true"),
        allow_from: None,
        once: false,
        until: None,
        schedule: None,
    };
    let out = run_granted(&g, &["true".into()]).unwrap();
    assert!(out.status.success());
}

#[test]
fn rejects_argv0_mismatch() {
    let g = Grant {
        tool: "true".into(),
        binary: PathBuf::from("/usr/bin/true"),
        allow_from: None,
        once: false,
        until: None,
        schedule: None,
    };
    let e = run_granted(&g, &["bash".into(), "-c".into(), "echo pwned".into()]).unwrap_err();
    assert!(e.to_string().contains("not added") || e.to_string().contains("not granted"));
}

#[test]
fn once_removed_after_success() {
    let mut s = empty_store();
    add(&mut s, "true", &[], true, None, None).unwrap();
    let g = check(&s, "true", &BodyId("server".into())).unwrap();
    run_granted(&g, &["true".into()]).unwrap();
    consume_once(&mut s, "true");
    assert!(check(&s, "true", &BodyId("server".into())).is_err());
}
