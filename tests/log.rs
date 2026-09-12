mod common;

use serde_json::json;

use common::paired;

#[tokio::test]
async fn log_on_both_bodies_after_success_and_deny() {
    let (laptop, server) = paired("laptop", "server").await;
    laptop.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    let v = server
        .rpc(json!({"op":"exec","body":"laptop","argv":["true"]}))
        .await
        .unwrap();
    assert_eq!(v["exit"], 0);
    let success_id = v["job"]["id"].as_str().unwrap().to_string();
    let v = server
        .rpc(json!({"op":"exec","body":"laptop","argv":["bash"]}))
        .await
        .unwrap();
    assert_eq!(v["status"], "denied");
    let denied_id = v["job"]["id"].as_str().unwrap().to_string();

    let server_log = server.rpc(json!({"op":"log"})).await.unwrap();
    let laptop_log = laptop.rpc(json!({"op":"log"})).await.unwrap();
    let mut server_lines = log_lines(&server_log);
    let mut laptop_lines = log_lines(&laptop_log);
    server_lines.sort();
    laptop_lines.sort();

    let mut expected = vec![
        format!("{success_id:?}  server [\"true\"] on laptop  exit 0"),
        format!("{denied_id:?}  server [\"bash\"] on laptop  denied: bash is not added on laptop"),
    ];
    // Each body may observe history in a different order. Compare the same
    // returned IDs and outcomes, without asserting a shared chronology.
    expected.sort();
    assert_eq!(server_lines, expected);
    assert_eq!(laptop_lines, server_lines);

    let by_id = |log: &serde_json::Value| -> std::collections::BTreeMap<String, serde_json::Value> {
        log["jobs"]
            .as_array()
            .expect("jobs")
            .iter()
            .map(|job| (job["id"].as_str().unwrap().to_string(), job.clone()))
            .collect()
    };
    let server_jobs = by_id(&server_log);
    let laptop_jobs = by_id(&laptop_log);
    assert_eq!(server_jobs, laptop_jobs);
    assert_eq!(server_jobs.len(), 2);
    for job in server_jobs.values() {
        let id = job["id"].as_str().expect("job id");
        assert!(is_uuid_v4(id), "job id should be uuid v4, got {id}");
    }
}

fn log_lines(v: &serde_json::Value) -> Vec<String> {
    v["lines"]
        .as_array()
        .expect("lines")
        .iter()
        .map(|x| x.as_str().expect("line").to_string())
        .collect()
}

fn is_uuid_v4(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 36
        && b[8] == b'-'
        && b[13] == b'-'
        && b[14] == b'4'
        && b[18] == b'-'
        && matches!(b[19], b'8' | b'9' | b'a' | b'b')
        && b[23] == b'-'
        && id.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}
