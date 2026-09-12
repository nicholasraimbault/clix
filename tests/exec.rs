use std::path::PathBuf;

use clix::{run_granted, Grant};

#[test]
fn runs_binary_not_shell() {
    let g = Grant {
        reservation: None,
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
        reservation: None,
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
