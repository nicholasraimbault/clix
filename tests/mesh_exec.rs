mod common;
use common::{paired, TestDaemon};
use serde_json::json;

#[tokio::test]
async fn server_runs_granted_binary_and_denies_bash() {
    let (laptop, server) = paired("laptop", "server").await;
    laptop.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    assert_eq!(
        server
            .rpc(json!({"op":"exec","body":"laptop","argv":["true"]}))
            .await
            .unwrap()["exit"],
        0
    );
    assert_eq!(
        server
            .rpc(json!({"op":"exec","body":"laptop","argv":["bash"]}))
            .await
            .unwrap()["status"],
        "denied"
    );
}

#[tokio::test]
async fn mesh_rejects_all_owner_and_result_injection_operations() {
    let (laptop, server) = paired("laptop", "server").await;
    for op in [
        "add",
        "remove",
        "allow",
        "deny",
        "allow_request",
        "deny_request",
        "pair",
        "pair_start",
        "pair_join",
        "job_poll",
        "job_result",
        "job_inspect",
        "job_output",
        "job_retry",
        "storage_status",
        "storage_prune",
        "pin_sync",
        "pin_conflicts",
        "pin_take_peer",
        "pin_recovery_list",
        "pin_recovery_inspect",
        "pin_recovery_export",
        "pin_recovery_resolve",
        "pin_recovery_discard",
    ] {
        let e = server
            .mesh_raw(
                laptop.mesh_addr(),
                json!({"op":op,"request_id":"req-1","tool":"true","from":"laptop","job":{"status":{"Done":{"exit":0}}}}),
            )
            .await
            .unwrap_err();
        assert!(e.to_string().contains("not allowed"), "{op}: {e}");
    }
    assert!(laptop.rpc(json!({"op":"hands"})).await.unwrap()["hands"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(laptop.rpc(json!({"op":"log"})).await.unwrap()["jobs"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn mesh_request_uses_authenticated_sender() {
    let (laptop, server) = paired("laptop", "server").await;
    server
        .mesh_raw(
            laptop.mesh_addr(),
            json!({"op":"request","tool":"true","from":"laptop"}),
        )
        .await
        .unwrap();
    let pending = laptop.rpc(json!({"op":"pending"})).await.unwrap();
    assert_eq!(pending["requests"][0]["from"], "server");
}

#[tokio::test]
async fn duplicate_invocation_returns_same_job_and_cannot_change_argv() {
    let (laptop, server) = paired("laptop", "server").await;
    laptop
        .rpc(json!({"op":"add","tool":"true","once":true}))
        .await
        .unwrap();
    let req = json!({"op":"exec","argv":["true"],"job_id":"redelivery","from":"laptop"});
    let first = server
        .mesh_raw(laptop.mesh_addr(), req.clone())
        .await
        .unwrap();
    assert_eq!(first["job"]["from"], "server");
    for _ in 0..50 {
        let next = server
            .mesh_raw(laptop.mesh_addr(), req.clone())
            .await
            .unwrap();
        if next["status"] == "done" {
            assert_eq!(next["exit"], 0);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let duplicate = server.mesh_raw(laptop.mesh_addr(), req).await.unwrap();
    assert_eq!(duplicate["status"], "done");
    assert!(server
        .mesh_raw(
            laptop.mesh_addr(),
            json!({"op":"exec","argv":["false"],"job_id":"redelivery"})
        )
        .await
        .is_err());
    let jobs = laptop.rpc(json!({"op":"log"})).await.unwrap();
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn third_peer_cannot_read_another_callers_job_or_spoof_allow_from() {
    let (laptop, server) = paired("laptop", "server").await;
    let phone = TestDaemon::spawn_named("phone").await;
    let phrase = laptop.rpc(json!({"op":"pair_start"})).await.unwrap()["phrase"]
        .as_str()
        .unwrap()
        .to_string();
    phone
        .rpc(json!({"op":"pair_join","phrase":phrase}))
        .await
        .unwrap();
    laptop
        .rpc(json!({"op":"add","tool":"true","allow":["phone"]}))
        .await
        .unwrap();
    let denied = server
        .mesh_raw(
            laptop.mesh_addr(),
            json!({"op":"exec","argv":["true"],"job_id":"restricted","from":"phone"}),
        )
        .await
        .unwrap();
    assert_eq!(denied["status"], "denied");
    assert!(denied["reason"].as_str().unwrap().contains("not allowed"));
    let private = phone
        .mesh_raw(
            laptop.mesh_addr(),
            json!({"op":"job_get","job_id":"restricted"}),
        )
        .await
        .unwrap();
    assert!(private["job"].is_null());
}

#[tokio::test]
async fn unknown_peer_is_dropped() {
    let laptop = TestDaemon::spawn_named("laptop").await;
    let stranger = TestDaemon::spawn_named("stranger").await;
    assert!(stranger
        .mesh_raw(laptop.mesh_addr(), json!({"op":"exec","argv":["true"]}))
        .await
        .is_err());
    assert!(laptop.rpc(json!({"op":"log"})).await.unwrap()["jobs"]
        .as_array()
        .unwrap()
        .is_empty());
}
