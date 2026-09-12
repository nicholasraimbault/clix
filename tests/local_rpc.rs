mod common;

use std::os::unix::fs::PermissionsExt;

use serde_json::json;

use common::TestDaemon;

#[tokio::test]
async fn add_then_hands() {
    let h = TestDaemon::spawn().await;
    h.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    let v = h.rpc(json!({"op":"hands"})).await.unwrap();
    assert!(v["hands"][0]["tool"] == "true");
}

#[tokio::test]
async fn owner_socket_is_mode_0600() {
    let h = TestDaemon::spawn().await;
    let mode = std::fs::metadata(&h.sock).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn missing_daemon_is_not_run_locally() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("clix.sock");
    let e = clix::client_send(&sock, json!({"op": "hands"})).unwrap_err();
    assert_eq!(e.to_string(), "Clix is not running. clix install");
}
