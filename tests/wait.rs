mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use serde_json::json;

use common::paired;

fn fast_poll() {
    std::env::set_var("CLIX_WAIT_POLL", "50ms");
}

#[tokio::test]
async fn wait_then_run_when_laptop_returns() {
    fast_poll();
    let (laptop, server) = paired("laptop", "server").await;
    laptop.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    laptop.kill().await;
    let handle = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .rpc(json!({"op":"exec","body":"laptop","argv":["true"]}))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _laptop = laptop.restart().await;
    let v = tokio::time::timeout(Duration::from_secs(15), handle)
        .await
        .expect("wait timed out")
        .unwrap()
        .unwrap();
    assert_eq!(v["exit"], 0);
}

#[tokio::test]
async fn unreachable_peer_is_asleep_not_connection_refused() {
    fast_poll();
    let (laptop, server) = paired("laptop", "server").await;
    laptop.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    laptop.kill().await;
    let v = server
        .rpc(json!({"op":"exec","body":"laptop","argv":["true"],"no_wait":true}))
        .await
        .expect("unreachable body waits; it does not fail the rpc");
    let dumped = v.to_string();
    let lower = dumped.to_lowercase();
    assert!(
        !lower.contains("connection refused"),
        "raw I/O reached the user: {dumped}"
    );
    assert!(
        !lower.contains("os error"),
        "raw I/O reached the user: {dumped}"
    );
    let waiting = v.get("waiting").and_then(|x| x.as_str()).unwrap_or("");
    assert!(
        waiting.contains("asleep") && waiting.contains("waiting"),
        "expected '{{body}} is asleep, waiting…', got {v}"
    );
    assert_eq!(v["status"], "waiting");
}

#[tokio::test]
async fn wait_runs_exactly_once_when_laptop_returns() {
    fast_poll();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let script = dir.path().join("markonce");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho x >> '{}'\n", marker.display()),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let (laptop, server) = paired("laptop", "server").await;
    laptop
        .rpc(json!({"op":"add","tool": script.to_str().unwrap(), "once": true}))
        .await
        .unwrap();
    laptop.kill().await;
    let handle = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .rpc(json!({"op":"exec","body":"laptop","argv":["markonce"]}))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _laptop = laptop.restart().await;
    let v = tokio::time::timeout(Duration::from_secs(15), handle)
        .await
        .expect("wait timed out")
        .unwrap()
        .unwrap();
    assert_eq!(v["exit"], 0, "{v}");
    let ran = std::fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(
        ran.matches('x').count(),
        1,
        "tool must run exactly once, got {ran:?}"
    );
    let again = server
        .rpc(json!({"op":"exec","body":"laptop","argv":["markonce"]}))
        .await
        .unwrap();
    assert_eq!(again["status"], "denied", "{again}");
}
