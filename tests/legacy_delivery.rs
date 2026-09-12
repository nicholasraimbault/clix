mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use clix::{BodyId, Job, JobStatus};
use serde_json::{json, Value};

use common::{paired, TestDaemon};

fn state(daemon: &TestDaemon) -> Value {
    serde_json::from_slice(
        &std::fs::read(daemon.pin_dir().parent().unwrap().join("state.json")).unwrap(),
    )
    .unwrap()
}

async fn legacy_delivery(status: JobStatus) {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let script = dir.path().join("legacy-mark");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'x\\n' >> '{}'\nprintf 'legacy resumed\\n'\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (laptop, server) = paired("laptop", "server").await;
    laptop
        .rpc(json!({"op":"add","tool":script,"allow":["server"],"once":true}))
        .await
        .unwrap();
    let id = "10000000-0000-4000-8000-000000000001";
    if status == JobStatus::Running {
        // The old runner completed, but the origin had not yet received its
        // terminal result when it stopped for upgrade.
        server
            .mesh_raw(
                laptop.mesh_addr(),
                json!({"op":"exec","job_id":id,"argv":["legacy-mark"]}),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state(&laptop)["jobs"][0]["status"] == json!({"Done":{"exit":0}}) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "x\n");
        assert!(state(&laptop)["grants"].as_array().unwrap().is_empty());
    }
    server.kill().await;
    laptop.kill().await;

    // Serialize the pre-history on-disk shape. Pair identity and the runner's
    // once grant/receipt come from actual daemon operations above.
    let mut old = state(&server);
    old["jobs"] = json!([Job {
        id: id.into(),
        from: BodyId("server".into()),
        body: BodyId("laptop".into()),
        argv: vec!["legacy-mark".into()],
        status,
        stdout: vec![],
        stderr: vec![],
    }]);
    for field in [
        "history",
        "job_certificates",
        "legacy_jobs",
        "retry_links",
        "output_pruned",
        "history_pruned",
        "format_version",
    ] {
        old.as_object_mut().unwrap().remove(field);
    }
    let path = server.pin_dir().parent().unwrap().join("state.json");
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

    let server = server.restart().await;
    let migrated = state(&server);
    let binding = migrated["job_certificates"][id]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(migrated["legacy_jobs"], json!([id]));
    assert_eq!(
        migrated["history"]["entries"][&binding]["record"]["certificate"]["invocation"]
            ["legacy_observer"],
        "server"
    );
    let laptop = laptop.restart().await;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let saved = state(&server);
            let result: Job = serde_json::from_value(saved["jobs"][0].clone()).unwrap();
            match result.status {
                JobStatus::Done { exit: 0 } => {
                    assert_eq!(result.id, id);
                    assert_eq!(result.stdout, b"legacy resumed\n");
                    break;
                }
                JobStatus::Failed { .. }
                | JobStatus::Denied { .. }
                | JobStatus::Uncertain { .. } => {
                    panic!("legacy admission did not resume: {:?}", result.status);
                }
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await
    .expect("upgraded legacy admission did not converge");

    let completed = state(&server);
    assert_eq!(completed["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(completed["job_certificates"][id], binding);
    assert_eq!(completed["owner_sk"], old["owner_sk"]);
    assert_eq!(completed["peers"], old["peers"]);
    assert!(completed["grants"].as_array().unwrap().is_empty());
    assert!(state(&laptop)["grants"].as_array().unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "x\n");

    // Legacy redelivery keeps the original receipt even though --once is gone.
    let duplicate = server
        .mesh_raw(
            laptop.mesh_addr(),
            json!({"op":"exec","job_id":id,"argv":["legacy-mark"]}),
        )
        .await
        .unwrap();
    assert_eq!(duplicate["exit"], 0);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "x\n");
    assert_eq!(state(&laptop)["jobs"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn upgraded_legacy_queued_job_resumes_with_original_id_and_once_grant() {
    legacy_delivery(JobStatus::Queued).await;
}

#[tokio::test]
async fn upgraded_legacy_waiting_job_resumes_with_original_id_and_once_grant() {
    legacy_delivery(JobStatus::WaitingBody).await;
}

#[tokio::test]
async fn upgraded_legacy_running_job_recovers_existing_result_without_rerunning() {
    legacy_delivery(JobStatus::Running).await;
}
