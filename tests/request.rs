mod common;

use serde_json::json;

use common::paired;

#[tokio::test]
async fn owner_decisions_require_and_keep_the_inspected_request_id() {
    let (laptop, server) = paired("laptop", "server").await;
    server
        .mesh_raw(laptop.mesh_addr(), json!({"op":"request", "tool":"true"}))
        .await
        .unwrap();
    let a = laptop.rpc(json!({"op":"pending"})).await.unwrap()["requests"][0]["id"].clone();
    server
        .mesh_raw(laptop.mesh_addr(), json!({"op":"request", "tool":"false"}))
        .await
        .unwrap();
    let before = laptop.rpc(json!({"op":"pending"})).await.unwrap();
    for op in ["allow_request", "deny_request", "allow", "deny"] {
        for req in [
            json!({"op":op}),
            json!({"op":op, "request_id":""}),
            json!({"op":op, "request_id":null}),
            json!({"op":op, "request_id":42}),
            json!({"op":op, "request_id":"unknown"}),
        ] {
            assert!(laptop.rpc(req).await.is_err());
            assert_eq!(laptop.rpc(json!({"op":"pending"})).await.unwrap(), before);
            assert!(laptop.rpc(json!({"op":"hands"})).await.unwrap()["hands"]
                .as_array()
                .unwrap()
                .is_empty());
        }
    }
    for op in ["allow", "deny"] {
        assert!(laptop.rpc(json!({"op":op, "request_id":a})).await.is_err());
        assert_eq!(laptop.rpc(json!({"op":"pending"})).await.unwrap(), before);
    }
    assert!(laptop
        .rpc(json!({"op":"allow_request", "request_id":a, "allow":["unknown"]}))
        .await
        .is_err());
    assert_eq!(laptop.rpc(json!({"op":"pending"})).await.unwrap(), before);
    laptop
        .rpc(json!({"op":"allow_request", "request_id":a}))
        .await
        .unwrap();
    let grants = laptop.rpc(json!({"op":"hands"})).await.unwrap();
    assert_eq!(grants["hands"][0]["tool"], "true");
    assert_eq!(grants["hands"][0]["allow_from"], json!(["server"]));
    assert_eq!(grants["hands"][0]["once"], true);
    let pending = laptop.rpc(json!({"op":"pending"})).await.unwrap();
    assert_eq!(pending["requests"].as_array().unwrap().len(), 1);
    assert_eq!(pending["requests"][0]["tool"], "false");
    assert!(laptop
        .rpc(json!({"op":"allow_request", "request_id":a}))
        .await
        .is_err());
    assert!(laptop
        .rpc(json!({"op":"deny_request", "request_id":a}))
        .await
        .is_err());
    assert_eq!(laptop.rpc(json!({"op":"pending"})).await.unwrap(), pending);
    laptop
        .rpc(json!({"op":"deny_request", "request_id":pending["requests"][0]["id"]}))
        .await
        .unwrap();
    assert_eq!(laptop.rpc(json!({"op":"hands"})).await.unwrap(), grants);
}

#[tokio::test]
async fn owner_grant_changes_invalidate_requests_and_keep_delivery_receipts() {
    let (laptop, server) = paired("laptop", "server").await;
    let delivery = json!({"op":"request", "tool":"true", "request_id":"before-owner-change"});
    server
        .mesh_raw(laptop.mesh_addr(), delivery.clone())
        .await
        .unwrap();
    let old = laptop.rpc(json!({"op":"pending"})).await.unwrap()["requests"][0]["id"].clone();
    laptop
        .rpc(json!({"op":"request", "body":"laptop", "tool":clix::resolve_tool("true").unwrap()}))
        .await
        .unwrap();
    server
        .mesh_raw(laptop.mesh_addr(), json!({"op":"request", "tool":"false"}))
        .await
        .unwrap();
    let before = laptop.rpc(json!({"op":"pending"})).await.unwrap();
    assert!(laptop
        .rpc(json!({"op":"add", "tool":"true", "allow":["unknown"]}))
        .await
        .is_err());
    assert_eq!(laptop.rpc(json!({"op":"pending"})).await.unwrap(), before);
    laptop
        .rpc(json!({"op":"add", "tool":"true", "allow":["laptop"]}))
        .await
        .unwrap();
    let grants = laptop.rpc(json!({"op":"hands"})).await.unwrap();
    let pending = laptop.rpc(json!({"op":"pending"})).await.unwrap();
    assert_eq!(pending["requests"].as_array().unwrap().len(), 1);
    assert_eq!(pending["requests"][0]["tool"], "false");
    assert!(laptop
        .rpc(json!({"op":"allow_request", "request_id":old}))
        .await
        .is_err());
    assert_eq!(laptop.rpc(json!({"op":"hands"})).await.unwrap(), grants);
    let laptop = laptop.restart().await;
    server.mesh_raw(laptop.mesh_addr(), delivery).await.unwrap();
    assert_eq!(laptop.rpc(json!({"op":"pending"})).await.unwrap(), pending);
    server
        .mesh_raw(laptop.mesh_addr(), json!({"op":"request", "tool":"true"}))
        .await
        .unwrap();
    let old = laptop.rpc(json!({"op":"pending"})).await.unwrap()["requests"][1]["id"].clone();
    laptop
        .rpc(json!({"op":"remove", "tool":"true"}))
        .await
        .unwrap();
    assert!(laptop
        .rpc(json!({"op":"allow_request", "request_id":old}))
        .await
        .is_err());
    assert_eq!(laptop.rpc(json!({"op":"pending"})).await.unwrap(), pending);
    assert!(laptop.rpc(json!({"op":"hands"})).await.unwrap()["hands"]
        .as_array()
        .unwrap()
        .is_empty());
}

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
    laptop
        .rpc(json!({"op": "allow_request", "request_id":p["requests"][0]["id"]}))
        .await
        .unwrap(); // once
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
        .mesh_raw(laptop.mesh_addr(), json!({"op": "allow_request"}))
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
async fn deny_drops_selected_request() {
    let (laptop, server) = paired("laptop", "server").await;
    let v = server
        .rpc(json!({"op": "exec", "body": "laptop", "argv": ["true"]}))
        .await
        .unwrap();
    assert_eq!(v["status"], "denied");
    let pending = laptop.rpc(json!({"op":"pending"})).await.unwrap();
    laptop
        .rpc(json!({"op": "deny_request", "request_id":pending["requests"][0]["id"]}))
        .await
        .unwrap();
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
        .mesh_raw(laptop.mesh_addr(), json!({"op": "deny_request"}))
        .await
        .unwrap_err();
    assert!(!e.to_string().is_empty());
}

#[tokio::test]
async fn local_request_can_be_allowed_once() {
    let daemon = common::TestDaemon::spawn_named("laptop").await;
    daemon
        .rpc(json!({"op":"request", "body":"laptop", "tool":"true"}))
        .await
        .unwrap();
    let pending = daemon.rpc(json!({"op":"pending"})).await.unwrap();
    daemon
        .rpc(json!({"op":"allow_request", "request_id":pending["requests"][0]["id"]}))
        .await
        .unwrap();
    let run = daemon
        .rpc(json!({"op":"exec", "body":"laptop", "argv":["true"]}))
        .await
        .unwrap();
    assert_eq!(run["exit"], 0);
    assert!(daemon.rpc(json!({"op":"hands"})).await.unwrap()["hands"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn request_redelivery_after_owner_denial_does_not_recreate_prompt() {
    let (laptop, server) = paired("laptop", "server").await;
    let request = json!({"op":"request", "tool":"true", "request_id":"delivery-one"});
    server
        .mesh_raw(laptop.mesh_addr(), request.clone())
        .await
        .unwrap();
    let pending = laptop.rpc(json!({"op":"pending"})).await.unwrap();
    laptop
        .rpc(json!({"op":"deny_request", "request_id":pending["requests"][0]["id"]}))
        .await
        .unwrap();
    server.mesh_raw(laptop.mesh_addr(), request).await.unwrap();
    assert!(
        laptop.rpc(json!({"op":"pending"})).await.unwrap()["requests"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(server
        .mesh_raw(
            laptop.mesh_addr(),
            json!({"op":"request", "tool":"false", "request_id":"delivery-one"})
        )
        .await
        .is_err());
}
