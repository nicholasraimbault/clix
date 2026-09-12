mod common;
use common::paired;
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_bidirectional_exec_keeps_room_for_pin_callbacks() {
    let (a, b) = paired("left", "right").await;
    a.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    b.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..8 {
        for (caller, runner, direction) in [
            (a.clone(), b.clone(), "right"),
            (b.clone(), a.clone(), "left"),
        ] {
            tasks.spawn(async move {
                let id = format!("{direction}-{n}");
                let request = json!({"op":"exec","argv":["true"],"job_id":id});
                tokio::time::timeout(Duration::from_secs(8), async {
                    loop {
                        match caller.mesh_raw(runner.mesh_addr(), request.clone()).await {
                            Ok(result) if result["status"] == "done" => {
                                assert_eq!(result["exit"], 0);
                                return;
                            }
                            Ok(result) => assert_eq!(result["status"], "running"),
                            Err(clix::ClixError::Capacity(_)) => {}
                            Err(e) => panic!("unexpected mesh result: {e}"),
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("reciprocal exec/pin calls must not deadlock");
            });
        }
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    for daemon in [a, b] {
        // Shared history may already include the other runner's records.
        // Execution deduplication is checked against actual local admissions.
        let state: serde_json::Value = serde_json::from_slice(
            &std::fs::read(daemon.pin_dir().parent().unwrap().join("state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["jobs"].as_array().unwrap().len(), 8);
    }
}
