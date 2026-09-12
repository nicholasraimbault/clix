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
    let v = server
        .rpc(json!({"op":"exec","body":"laptop","argv":["bash"]}))
        .await
        .unwrap();
    assert_eq!(v["status"], "denied");

    let server_log = server.rpc(json!({"op":"log"})).await.unwrap();
    let laptop_log = laptop.rpc(json!({"op":"log"})).await.unwrap();
    let server_lines = log_lines(&server_log);
    let laptop_lines = log_lines(&laptop_log);

    assert_eq!(
        server_lines,
        vec![
            "server true on laptop  exit 0".to_string(),
            "server bash on laptop  denied: bash is not added on laptop".to_string(),
        ]
    );
    assert_eq!(laptop_lines, server_lines);

    let server_jobs = server_log["jobs"].as_array().expect("jobs");
    let laptop_jobs = laptop_log["jobs"].as_array().expect("jobs");
    assert_eq!(server_jobs, laptop_jobs);
    assert_eq!(server_jobs.len(), 2);
    for job in server_jobs {
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
