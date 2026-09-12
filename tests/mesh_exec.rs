mod common;

use serde_json::json;

use common::{paired, TestDaemon};

#[tokio::test]
async fn server_runs_adb_after_laptop_add() {
    let (laptop, server) = paired("laptop", "server").await;
    laptop.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    let v = server
        .rpc(json!({"op":"exec","body":"laptop","argv":["true"]}))
        .await
        .unwrap();
    assert_eq!(v["exit"], 0);
}

#[tokio::test]
async fn server_cannot_add_on_laptop() {
    let (laptop, server) = paired("laptop", "server").await;
    let e = server
        .mesh_raw(laptop.mesh_addr(), json!({"op":"add","tool":"true"}))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not allowed") || e.to_string().contains("owner"));
}

#[tokio::test]
async fn bash_denied_without_add() {
    let (laptop, server) = paired("laptop", "server").await;
    let _ = laptop.mesh_addr();
    let v = server
        .rpc(json!({"op":"exec","body":"laptop","argv":["bash"]}))
        .await
        .unwrap();
    assert_eq!(v["status"], "denied");
    assert!(v["reason"].as_str().unwrap().contains("not added"));
}

#[tokio::test]
async fn mesh_rejects_owner_ops() {
    let (laptop, server) = paired("laptop", "server").await;
    for op in ["remove", "allow", "deny", "pair"] {
        let mut req = json!({"op": op});
        if op == "remove" {
            req["tool"] = json!("true");
        }
        let e = server.mesh_raw(laptop.mesh_addr(), req).await.unwrap_err();
        let s = e.to_string();
        assert!(
            s.contains("not allowed") || s.contains("owner"),
            "{op}: {s}"
        );
    }
}

#[tokio::test]
async fn mesh_allows_request_and_job_poll() {
    let (laptop, server) = paired("laptop", "server").await;
    for op in ["request", "job_poll"] {
        server
            .mesh_raw(laptop.mesh_addr(), json!({"op": op}))
            .await
            .expect(op);
    }
}

#[tokio::test]
async fn mesh_from_is_the_authenticated_peer() {
    let (laptop, server) = paired("laptop", "server").await;
    laptop
        .rpc(json!({"op": "add", "tool": "true", "allow": ["laptop"]}))
        .await
        .unwrap();
    let v = server
        .rpc(json!({"op": "exec", "body": "laptop", "argv": ["true"]}))
        .await
        .unwrap();
    assert_eq!(v["status"], "denied");
    assert!(v["reason"].as_str().unwrap().contains("not allowed"));
}

#[tokio::test]
async fn unknown_peer_is_dropped() {
    let laptop = TestDaemon::spawn_named("laptop").await;
    let stranger = TestDaemon::spawn_named("stranger").await;
    let e = stranger
        .mesh_raw(laptop.mesh_addr(), json!({"op": "exec", "argv": ["true"]}))
        .await
        .unwrap_err();
    let s = e.to_string();
    assert!(
        s.contains("unknown") || s.contains("dropped") || s.contains("peer"),
        "{s}"
    );
}
