//! Retention, replay protection and completion-capacity regressions.
use crate::{history, storage, BodyId, ClixError, Job, JobStatus, Peer, Request, Store};
use ed25519_dalek::SigningKey;

const ID: &str = "00000000-0000-4000-8000-000000000001";
fn key(seed: u8) -> Vec<u8> {
    SigningKey::from_bytes(&[seed; 32])
        .verifying_key()
        .to_bytes()
        .to_vec()
}
fn fixture(name: &str, seed: u8) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.body_name = name.into();
    store.owner_sk = vec![seed; 32];
    store.peers = [("origin", 1), ("runner", 2)]
        .into_iter()
        .filter(|(n, _)| *n != name)
        .map(|(n, k)| Peer {
            name: BodyId(n.into()),
            owner_pk: key(k),
            addr: None,
        })
        .collect();
    store.update(|_| Ok(())).unwrap();
    (dir, store)
}
fn admitted_result() -> (tempfile::TempDir, Store, history::Published) {
    let (dir, mut origin) = fixture("origin", 1);
    let (_runner_dir, mut runner) = fixture("runner", 2);
    let job = Job {
        id: ID.into(),
        from: BodyId("origin".into()),
        body: BodyId("runner".into()),
        argv: vec!["true".into()],
        status: JobStatus::Queued,
        stdout: vec![],
        stderr: vec![],
    };
    origin
        .update(|s| {
            s.jobs.push(job.clone());
            Ok(())
        })
        .unwrap();
    let c = history::for_job(&origin, &job)
        .unwrap()
        .record
        .certificate
        .clone();
    runner
        .update(|s| {
            history::bind_admission(s, &c)?;
            let mut j = job.clone();
            j.status = JobStatus::Running;
            s.jobs.push(j);
            Ok(())
        })
        .unwrap();
    runner
        .update(|s| {
            s.jobs[0].status = JobStatus::Done { exit: 0 };
            s.jobs[0].stdout = b"retained result".to_vec();
            Ok(())
        })
        .unwrap();
    let p = history::export_entry(&runner, history::for_job(&runner, &runner.jobs[0]).unwrap());
    (dir, origin, p)
}
#[test]
fn keeping_one_output_job_preserves_one_remote_result() {
    let (_dir, mut origin, result) = admitted_result();
    origin
        .update(|s| history::accept_runner_reply(s, &BodyId("runner".into()), &result))
        .unwrap();
    let removed = origin.update(|s| storage::prune(s, 1)).unwrap();
    assert_eq!(removed, 0, "one invocation must count as one output job");
    assert_eq!(history::views(&origin)[0].job.stdout, b"retained result");
    assert_eq!(storage::output_bytes(&origin), b"retained result".len());
}
#[test]
fn polling_a_locally_pruned_replica_does_not_rehydrate_admitted_payload() {
    let (dir, mut origin, result) = admitted_result();
    origin.update(|s| history::ingest(s, &result)).unwrap();
    origin.update(|s| storage::prune(s, 0)).unwrap();
    assert_eq!(storage::output_bytes(&origin), 0);
    origin
        .update(|s| history::accept_runner_reply(s, &BodyId("runner".into()), &result))
        .unwrap();
    assert_eq!(
        storage::output_bytes(&origin),
        0,
        "poll reply must honor the owner's retained-output tombstone"
    );
    assert_eq!(origin.jobs[0].status, JobStatus::Done { exit: 0 });
    assert!(origin.output_pruned.contains(ID));
    assert!(!history::views(&origin)[0].output_available);
    let origin = Store::open(dir.path()).unwrap();
    assert_eq!(storage::output_bytes(&origin), 0);
    assert_eq!(origin.jobs.len(), 1);
    assert_eq!(origin.job_certificates.len(), 1);
}
#[test]
fn presave_capacity_rejection_does_not_poison_or_change_durable_state() {
    let (dir, mut store) = fixture("origin", 1);
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let error = store
        .update(|s| {
            for n in 0..1025 {
                s.requests.push(Request {
                    id: format!("r{n}"),
                    from: BodyId("runner".into()),
                    tool: format!("tool{n}"),
                    once_suggested: true,
                });
            }
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(error, ClixError::Capacity(_)));
    store.ensure_writable().unwrap();
    assert!(store.requests.is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    store.update(|_| Ok(())).unwrap();
}
#[test]
fn pruning_retained_zeros_cannot_increase_the_completion_capacity_charge() {
    let (_dir, mut store) = fixture("origin", 1);
    // Complete both local jobs through the production publication hook.
    store
        .update(|s| {
            for n in 1..=2 {
                s.jobs.push(Job {
                    id: format!("zero-{n}"),
                    from: BodyId("origin".into()),
                    body: BodyId("origin".into()),
                    argv: vec!["true".into()],
                    status: JobStatus::Done { exit: 0 },
                    stdout: vec![0; 4 * 1024 * 1024],
                    stderr: vec![0; 4 * 1024 * 1024],
                });
            }
            Ok(())
        })
        .unwrap();
    let encoded_before = storage::encode(&store).unwrap().len();
    let old = store.clone();
    storage::prune(&mut store, 0).unwrap();
    let encoded_after = storage::encode(&store).unwrap().len();
    // Add hypothetical metadata equally to both measured encodings. This tests
    // the capacity arithmetic without allocating a 90 MiB padding field.
    let metadata = 90 * 1024 * 1024;
    let before = storage::completion_charge(&old, encoded_before + metadata);
    let after = storage::completion_charge(&store, encoded_after + metadata);
    assert!(
        after <= before,
        "pruning reduced serialized state but increased capacity charge"
    );
}
#[test]
fn upgraded_overquota_state_can_shrink_and_settle_an_existing_job() {
    let (_dir, mut store) = fixture("origin", 1);
    store
        .update(|s| {
            s.jobs.push(Job {
                id: ID.into(),
                from: BodyId("origin".into()),
                body: BodyId("origin".into()),
                argv: vec!["true".into()],
                status: JobStatus::Running,
                stdout: vec![],
                stderr: vec![],
            });
            Ok(())
        })
        .unwrap();
    // Represent pre-policy metadata without generating thousands of unrelated
    // history facts. The persisted state remains below the 128 MiB read limit.
    store.pin_index.last_error = Some("x".repeat(65 * 1024 * 1024));
    store.save().unwrap();
    store
        .update(|s| {
            let n = s.pin_index.last_error.as_ref().unwrap().len();
            s.pin_index.last_error.as_mut().unwrap().truncate(n - 512);
            Ok(())
        })
        .expect("an over-quota upgrade must still allow shrinking metadata");
    assert!(matches!(
        store.update(|s| {
            let mut job = s.jobs[0].clone();
            job.id = "new-work".into();
            job.status = JobStatus::Queued;
            s.jobs.push(job);
            Ok(())
        }),
        Err(ClixError::Capacity(_))
    ));
    store
        .update(|s| {
            s.jobs[0].status = JobStatus::Uncertain {
                reason: "recovered after restart".into(),
            };
            Ok(())
        })
        .expect("an admitted job must still settle while reducing its reserved charge");
    assert!(matches!(store.jobs[0].status, JobStatus::Uncertain { .. }));
    store.ensure_writable().unwrap();
}

#[test]
fn pruning_across_decimal_sequence_boundaries_cannot_increase_charge() {
    for boundary in [9, 99, 999, 9999, 9_999_999_999_999_999_999] {
        let (_dir, mut store) = fixture("origin", 1);
        store
            .update(|s| {
                for n in 1..=2 {
                    s.jobs.push(Job {
                        id: format!("boundary-{n}"),
                        from: BodyId("origin".into()),
                        body: BodyId("origin".into()),
                        argv: vec!["true".into()],
                        status: JobStatus::Done { exit: 0 },
                        stdout: vec![b'x'],
                        stderr: vec![],
                    });
                }
                Ok(())
            })
            .unwrap();
        // A previous prune makes both receipt sets nonempty: adding another
        // receipt then consumes its reserved comma exactly, without spare bytes.
        store.update(|s| storage::prune(s, 1)).unwrap();
        let remaining = store
            .jobs
            .iter()
            .find(|j| !j.stdout.is_empty())
            .unwrap()
            .id
            .clone();
        let key = store.job_certificates[&remaining].clone();
        store
            .update(|s| {
                s.history.clock = boundary;
                s.history.entries.get_mut(&key).unwrap().change = boundary;
                Ok(())
            })
            .unwrap();
        let before = storage::completion_charge(&store, storage::encode(&store).unwrap().len());
        store.update(|s| storage::prune(s, 0)).unwrap();
        let after = storage::completion_charge(&store, storage::encode(&store).unwrap().len());
        assert_eq!(store.history.clock, boundary + 1);
        assert_eq!(store.history.entries[&key].change, boundary + 1);
        assert!(
            after <= before,
            "pruning at {boundary} increased charge from {before} to {after}"
        );
    }
}

#[test]
fn execution_receipt_cap_refuses_new_admissions_at_the_limit() {
    let (_dir, mut store) = fixture("origin", 1);
    // Fill to exactly the cap in one durable update.
    store
        .update(|s| {
            for n in 0..storage::MAX_JOBS {
                s.jobs.push(Job {
                    id: format!("job-{n}"),
                    from: BodyId("origin".into()),
                    body: BodyId("origin".into()),
                    argv: vec!["true".into()],
                    status: JobStatus::Done { exit: 0 },
                    stdout: vec![],
                    stderr: vec![],
                });
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(store.jobs.len(), storage::MAX_JOBS);
    // status reports the same limit prepare enforces (single source of truth).
    assert_eq!(
        storage::status(&store)["execution_receipt_limit"],
        serde_json::json!(storage::MAX_JOBS)
    );
    // One more row is refused as a capacity error, state unchanged.
    let error = store
        .update(|s| {
            s.jobs.push(Job {
                id: "one-too-many".into(),
                from: BodyId("origin".into()),
                body: BodyId("origin".into()),
                argv: vec!["true".into()],
                status: JobStatus::Done { exit: 0 },
                stdout: vec![],
                stderr: vec![],
            });
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(error, ClixError::Capacity(_)), "{error}");
    assert_eq!(store.jobs.len(), storage::MAX_JOBS);
}
