mod common;

use serde_json::json;

use common::paired;

#[tokio::test]
async fn denied_becomes_pending_allow_once() {
    let (laptop, server) = paired("laptop", "server").await;
    let v = server
        .rpc(json!({"op": "exec", "body": "laptop", "argv": ["true"]}))
        .await
        .unwrap();
    assert_eq!(v["status"], "denied");
    let p = laptop.rpc(json!({"op": "pending"})).await.unwrap();
    assert_eq!(p["requests"][0]["tool"], "true");
    laptop.rpc(json!({"op": "allow"})).await.unwrap(); // once
    let v = server
        .rpc(json!({"op": "exec", "body": "laptop", "argv": ["true"]}))
        .await
        .unwrap();
    assert_eq!(v["exit"], 0);
    let v = server
        .rpc(json!({"op": "exec", "body": "laptop", "argv": ["true"]}))
        .await
        .unwrap();
    assert_eq!(v["status"], "denied"); // once consumed
}

#[tokio::test]
async fn server_cannot_allow() {
    let (laptop, server) = paired("laptop", "server").await;
    let e = server
        .mesh_raw(laptop.mesh_addr(), json!({"op": "allow"}))
        .await
        .unwrap_err();
    assert!(!e.to_string().is_empty());
}

#[tokio::test]
async fn request_creates_pending_without_running() {
    let (laptop, server) = paired("laptop", "server").await;
    server
        .rpc(json!({"op": "request", "body": "laptop", "tool": "true"}))
        .await
        .unwrap();
    let p = laptop.rpc(json!({"op": "pending"})).await.unwrap();
    assert_eq!(p["requests"][0]["tool"], "true");
    assert_eq!(p["requests"][0]["from"], "server");
    assert_eq!(p["requests"][0]["once_suggested"], true);
    let log = laptop.rpc(json!({"op": "log"})).await.unwrap();
    let jobs = log["jobs"].as_array().cloned().unwrap_or_default();
    assert!(
        jobs.iter().all(|j| j["status"].get("Done").is_none()),
        "request must not run the tool: {log}"
    );
}

#[tokio::test]
async fn deny_drops_latest() {
    let (laptop, server) = paired("laptop", "server").await;
    let v = server
        .rpc(json!({"op": "exec", "body": "laptop", "argv": ["true"]}))
        .await
        .unwrap();
    assert_eq!(v["status"], "denied");
    laptop.rpc(json!({"op": "deny"})).await.unwrap();
    let p = laptop.rpc(json!({"op": "pending"})).await.unwrap();
    let n = p["requests"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(n, 0, "{p}");
}

#[tokio::test]
async fn pending_is_on_the_target_body() {
    let (laptop, server) = paired("laptop", "server").await;
    let v = server
        .rpc(json!({"op": "exec", "body": "laptop", "argv": ["true"]}))
        .await
        .unwrap();
    assert_eq!(v["status"], "denied");
    let on_laptop = laptop.rpc(json!({"op": "pending"})).await.unwrap();
    assert_eq!(on_laptop["requests"][0]["tool"], "true");
    assert_eq!(on_laptop["requests"][0]["from"], "server");
    let on_server = server.rpc(json!({"op": "pending"})).await.unwrap();
    let n = on_server["requests"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(n, 0, "request must not live on the caller: {on_server}");
}

#[tokio::test]
async fn mesh_request_from_is_the_authenticated_peer() {
    let (laptop, server) = paired("laptop", "server").await;
    server
        .mesh_raw(
            laptop.mesh_addr(),
            json!({"op": "request", "tool": "true", "from": "god"}),
        )
        .await
        .unwrap();
    let p = laptop.rpc(json!({"op": "pending"})).await.unwrap();
    assert_eq!(p["requests"][0]["from"], "server", "{p}");
    assert_ne!(p["requests"][0]["from"], "god");
}

#[tokio::test]
async fn server_cannot_deny() {
    let (laptop, server) = paired("laptop", "server").await;
    let e = server
        .mesh_raw(laptop.mesh_addr(), json!({"op": "deny"}))
        .await
        .unwrap_err();
    assert!(!e.to_string().is_empty());
}
