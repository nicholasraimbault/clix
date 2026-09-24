mod common;

use std::sync::{Arc, Mutex};

use serde_json::json;

use clix::set_notify_hook;

use common::paired;

#[tokio::test]
async fn denied_exec_notifies_once_allow_deny() {
    std::env::set_var("CLIX_NOTIFY", "0");
    let calls = Arc::new(Mutex::new(Vec::<(String, String, Vec<String>)>::new()));
    let rec = calls.clone();
    set_notify_hook(Some(Box::new(move |r, actions| {
        rec.lock().unwrap().push((
            r.from.0.clone(),
            r.tool.clone(),
            actions.iter().map(|s| (*s).to_string()).collect(),
        ));
    })));

    let (_laptop, server) = paired("laptop", "server").await;
    let v = server
        .rpc(json!({"op": "exec", "body": "laptop", "argv": ["true"]}))
        .await
        .unwrap();
    assert_eq!(v["status"], "denied");

    for _ in 0..50 {
        if !calls.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let got = calls.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, "server");
    assert_eq!(got[0].1, "true");
    assert_eq!(
        got[0].2,
        vec!["once", "allow", "deny"],
        "notify actions: {got:?}"
    );
    assert_eq!(
        format!("{} wants {} on this machine", got[0].0, got[0].1),
        "server wants true on this machine"
    );
}

#[test]
fn display_detection_finds_a_compositor_socket_when_env_is_empty() {
    use std::path::Path;
    let rt = tempfile::tempdir().unwrap();
    let x11 = tempfile::tempdir().unwrap();
    // Nothing set anywhere: headless, no display.
    assert!(!clix::display_present_in(
        None,
        None,
        Some(rt.path()),
        x11.path()
    ));
    // The daemon started before login so its env is empty, but a Wayland
    // compositor is now running and left its socket in XDG_RUNTIME_DIR.
    std::fs::write(rt.path().join("wayland-0"), b"").unwrap();
    std::fs::write(rt.path().join("wayland-0.lock"), b"").unwrap();
    assert!(clix::display_present_in(
        None,
        None,
        Some(rt.path()),
        x11.path()
    ));
    // A live X server socket is detected the same way.
    let rt2 = tempfile::tempdir().unwrap();
    std::fs::write(x11.path().join("X0"), b"").unwrap();
    assert!(clix::display_present_in(
        None,
        None,
        Some(rt2.path()),
        x11.path()
    ));
    // An explicit env var short-circuits (the ordinary desktop case).
    let empty = Path::new("/nonexistent-clix-test-dir");
    assert!(clix::display_present_in(
        Some(std::ffi::OsStr::new(":0")),
        None,
        Some(empty),
        empty
    ));
}
