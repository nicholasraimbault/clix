//! Three disposable sidecars using production owner RPC, pairing, TLS and pull replication.
//! This is one-host integration evidence, not physical three-machine evidence.
mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use clix::{Job, JobStatus};
use common::TestDaemon;
use serde_json::{json, Value};

async fn pair(a: &TestDaemon, b: &TestDaemon) {
    let invitation = a.rpc(json!({"op":"pair_start"})).await.unwrap();
    b.rpc(json!({"op":"pair_join","phrase":invitation["phrase"]}))
        .await
        .unwrap();
}

async fn full_mesh() -> (TestDaemon, TestDaemon, TestDaemon) {
    let origin = TestDaemon::spawn_named("origin").await;
    let runner = TestDaemon::spawn_named("runner").await;
    let observer = TestDaemon::spawn_named("observer").await;
    pair(&origin, &runner).await;
    pair(&runner, &observer).await;
    pair(&origin, &observer).await;
    (origin, runner, observer)
}

fn read_state(daemon: &TestDaemon) -> Value {
    // Read an atomically replaced state snapshot; never open it as a writable Store.
    serde_json::from_slice(
        &fs::read(daemon.pin_dir().parent().unwrap().join("state.json")).unwrap(),
    )
    .unwrap()
}

fn authority(state: &Value) -> Value {
    json!({"jobs":state["jobs"],"grants":state["grants"],"requests":state["requests"],
        "outbound_requests":state["outbound_requests"],"request_receipts":state["request_receipts"],
        "job_certificates":state["job_certificates"],"retry_links":state["retry_links"],
        "legacy_jobs":state["legacy_jobs"]})
}

async fn wait_log(
    daemon: &TestDaemon,
    description: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let log = tokio::time::timeout(Duration::from_secs(3), daemon.rpc(json!({"op":"log"})))
            .await
            .expect("owner log RPC timed out")
            .unwrap();
        if predicate(&log) {
            return log;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}: {log}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn logged_job(log: &Value, id: &str) -> Option<Job> {
    log["jobs"]
        .as_array()?
        .iter()
        .find(|j| j["id"] == id)
        .map(|j| serde_json::from_value(j.clone()).unwrap())
}

fn peer_diagnostic<'a>(log: &'a Value, name: &str) -> Option<&'a Value> {
    log["history"]["peers"]
        .as_array()?
        .iter()
        .find(|p| p["body"] == name)
}

async fn local_printf(origin: &TestDaemon, text: &str) -> Job {
    origin
        .rpc(json!({"op":"add","tool":"printf"}))
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        origin.rpc(json!({"op":"exec","body":"origin","argv":["printf","%s",text]})),
    )
    .await
    .expect("local execution timed out")
    .unwrap();
    assert_eq!(result["exit"], 0, "{result}");
    serde_json::from_value(result["job"].clone()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_only_result_reaches_third_body_through_relay_with_origin_offline() {
    let (origin, relay, observer) = full_mesh().await;
    let before = authority(&read_state(&observer));
    observer.kill().await;
    let result = local_printf(&origin, "origin-only result\n").await;
    wait_log(&relay, "relay to retain origin's local result", |log| {
        logged_job(log, &result.id).as_ref() == Some(&result)
    })
    .await;
    assert!(read_state(&relay)["jobs"].as_array().unwrap().is_empty());
    origin.kill().await;
    assert!(origin.rpc(json!({"op":"log"})).await.is_err());
    let observer = observer.restart().await;
    let log = wait_log(
        &observer,
        "offline-origin result relayed to observer",
        |log| logged_job(log, &result.id).as_ref() == Some(&result),
    )
    .await;
    assert_eq!(authority(&read_state(&observer)), before);
    let view = log["views"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["job"]["id"] == result.id)
        .unwrap();
    assert_eq!(view["provenance"], "verified runner result");
    assert_eq!(view["output_available"], true);
    wait_log(&observer, "origin unavailability diagnostic", |log| {
        peer_diagnostic(log, "origin")
            .is_some_and(|p| p["complete"] == false && p["error"].is_string())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn origin_pin_conflict_failure_replicates_without_runner_admission() {
    let (origin, runner, observer) = full_mesh().await;
    runner.rpc(json!({"op":"add","tool":"true"})).await.unwrap();
    let runner_before = authority(&read_state(&runner));
    let observer_before = authority(&read_state(&observer));
    // Pairing has completed with empty trees; these are independent new edits.
    fs::create_dir_all(origin.pin_dir()).unwrap();
    fs::create_dir_all(runner.pin_dir()).unwrap();
    fs::write(origin.pin_dir().join("conflict.txt"), b"origin edit").unwrap();
    fs::write(runner.pin_dir().join("conflict.txt"), b"runner edit").unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        origin.rpc(json!({"op":"exec","body":"runner","argv":["true"]})),
    )
    .await
    .expect("pin conflict execution timed out")
    .unwrap();
    assert_eq!(response["status"], "failed", "{response}");
    assert!(response["reason"].as_str().unwrap().contains("Not merging"));
    let failed: Job = serde_json::from_value(response["job"].clone()).unwrap();
    for daemon in [&runner, &observer] {
        let log = wait_log(daemon, "origin delivery failure", |log| {
            logged_job(log, &failed.id).as_ref() == Some(&failed)
        })
        .await;
        let view = log["views"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["job"]["id"] == failed.id)
            .unwrap();
        assert_eq!(
            view["provenance"],
            "origin delivery observation; runner outcome not known"
        );
    }
    assert_eq!(authority(&read_state(&runner)), runner_before);
    assert_eq!(authority(&read_state(&observer)), observer_before);
    assert_eq!(
        fs::read(origin.pin_dir().join("conflict.txt")).unwrap(),
        b"origin edit"
    );
    assert_eq!(
        fs::read(runner.pin_dir().join("conflict.txt")).unwrap(),
        b"runner edit"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waiting_and_running_replicas_never_become_third_body_execution() {
    let (origin, runner, observer) = full_mesh().await;
    let temporary = tempfile::tempdir().unwrap();
    let script = temporary.path().join("held");
    let started = temporary.path().join("started");
    let release = temporary.path().join("release");
    // Fixed-format script, with disposable paths supplied as argv rather than shell text.
    fs::write(&script,b"#!/bin/sh\nprintf 'started\\n' >> \"$1\"\nn=0\nwhile [ ! -f \"$2\" ]; do\n n=$((n + 1))\n [ \"$n\" -lt 1000 ] || exit 9\n sleep 0.02\ndone\nprintf 'released\\n'\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    runner.rpc(json!({"op":"add","tool":script})).await.unwrap();
    let before = authority(&read_state(&observer));
    runner.kill().await;
    let response = origin
        .rpc(json!({"op":"exec","body":"runner","argv":["held",started,release],"no_wait":true}))
        .await
        .unwrap();
    let id = response["job"]["id"].as_str().unwrap().to_string();
    wait_log(
        &observer,
        "queued work while its runner is offline",
        |log| {
            logged_job(log, &id)
                .is_some_and(|j| matches!(j.status, JobStatus::Queued | JobStatus::WaitingBody))
        },
    )
    .await;
    assert_eq!(authority(&read_state(&observer)), before);
    assert!(!started.exists());
    let _runner = runner.restart().await;
    wait_log(&observer, "signed running result", |log| {
        logged_job(log, &id).is_some_and(|j| j.status == JobStatus::Running)
    })
    .await;
    assert_eq!(authority(&read_state(&observer)), before);
    assert_eq!(fs::read_to_string(&started).unwrap(), "started\n");
    fs::write(&release, b"continue").unwrap();
    let log = wait_log(&observer, "runner completion", |log| {
        logged_job(log, &id).is_some_and(|j| j.status == JobStatus::Done { exit: 0 })
    })
    .await;
    assert_eq!(logged_job(&log, &id).unwrap().stdout, b"released\n");
    assert_eq!(authority(&read_state(&observer)), before);
    assert_eq!(fs::read_to_string(&started).unwrap(), "started\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_author_stays_incomplete_until_explicit_pairing_and_rescan() {
    let origin = TestDaemon::spawn_named("origin").await;
    let relay = TestDaemon::spawn_named("runner").await;
    let observer = TestDaemon::spawn_named("observer").await;
    pair(&origin, &relay).await;
    pair(&relay, &observer).await;
    let before = authority(&read_state(&observer));
    let result = local_printf(&origin, "known only to relay\n").await;
    wait_log(&relay, "relay to retain result", |log| {
        logged_job(log, &result.id).as_ref() == Some(&result)
    })
    .await;
    let log = wait_log(&observer, "honest unknown-author diagnostic", |log| {
        peer_diagnostic(log, "runner")
            .is_some_and(|p| p["unknown_authors"] == true && p["complete"] == false)
    })
    .await;
    assert!(logged_job(&log, &result.id).is_none());
    assert_eq!(authority(&read_state(&observer)), before);
    pair(&origin, &observer).await;
    // Pairing authorizes verification; it does not create local admission.
    wait_log(&observer, "explicit pairing and relay rescan", |log| {
        logged_job(log, &result.id).as_ref() == Some(&result)
            && peer_diagnostic(log, "runner")
                .is_some_and(|p| p["complete"] == true && p["unknown_authors"] == false)
    })
    .await;
    assert_eq!(authority(&read_state(&observer)), before);
}
