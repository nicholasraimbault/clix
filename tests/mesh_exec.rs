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
async fn job_poll_marks_waiting_claimed() {
    let (laptop, server) = paired("laptop", "server").await;
    laptop.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    laptop.kill().await;
    let w = server
        .rpc(json!({"op":"exec","body":"laptop","argv":["true"],"no_wait":true}))
        .await
        .unwrap();
    assert_eq!(w["status"], "waiting");
    let v = laptop
        .mesh_raw(server.mesh_addr(), json!({"op": "job_poll"}))
        .await
        .unwrap();
    let jobs = v["jobs"].as_array().expect("jobs");
    assert_eq!(jobs.len(), 1, "{v}");
    assert_eq!(jobs[0]["status"], "Running");
    let v2 = laptop
        .mesh_raw(server.mesh_addr(), json!({"op": "job_poll"}))
        .await
        .unwrap();
    assert!(
        v2["jobs"].as_array().map(|a| a.is_empty()).unwrap_or(false),
        "claimed jobs must not be returned again: {v2}"
    );
}

#[tokio::test]
async fn job_result_only_finishes_job_destined_to_that_peer() {
    let (laptop, server) = paired("laptop", "server").await;
    laptop.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    laptop.kill().await;
    let w = server
        .rpc(json!({"op":"exec","body":"laptop","argv":["true"],"no_wait":true}))
        .await
        .unwrap();
    let id = w["job"]["id"].as_str().expect("job id").to_string();

    let phone = TestDaemon::spawn_named("phone").await;
    let phrase = server.rpc(json!({"op": "pair_start"})).await.unwrap()["phrase"]
        .as_str()
        .unwrap()
        .to_string();
    phone
        .rpc(json!({"op": "pair_join", "phrase": phrase}))
        .await
        .unwrap();
    phone
        .mesh_raw(
            server.mesh_addr(),
            json!({
                "op": "job_result",
                "result": {
                    "job": {
                        "id": id,
                        "body": "laptop",
                        "argv": ["true"],
                        "from": "phone",
                        "status": {"Done": {"exit": 0}}
                    }
                }
            }),
        )
        .await
        .unwrap();

    let log = server.rpc(json!({"op": "log"})).await.unwrap();
    let job = log["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == id)
        .expect("job");
    assert_eq!(job["from"], "server", "{job}");
    assert_ne!(job["status"], json!({"Done": {"exit": 0}}), "{job}");
}

#[tokio::test]
async fn job_result_from_field_does_not_become_caller() {
    let (laptop, server) = paired("laptop", "server").await;
    laptop.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    laptop.kill().await;
    let w = server
        .rpc(json!({"op":"exec","body":"laptop","argv":["true"],"no_wait":true}))
        .await
        .unwrap();
    let id = w["job"]["id"].as_str().expect("job id").to_string();
    laptop
        .mesh_raw(
            server.mesh_addr(),
            json!({
                "op": "job_result",
                "result": {
                    "job": {
                        "id": id,
                        "body": "laptop",
                        "argv": ["true"],
                        "from": "laptop",
                        "status": {"Done": {"exit": 0}}
                    }
                }
            }),
        )
        .await
        .unwrap();
    let log = server.rpc(json!({"op": "log"})).await.unwrap();
    let job = log["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == id)
        .expect("job");
    assert_eq!(
        job["from"], "server",
        "from in mesh JSON must not replace caller: {job}"
    );
    assert_eq!(job["status"], json!({"Done": {"exit": 0}}), "{job}");
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
