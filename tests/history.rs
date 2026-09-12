//! History authority and retention regressions using disposable state.

use clix::history::{
    self, CapturedOutput, Certificate, Fact, Invocation, OutputRef, Published, Record, Role,
    SignedFact,
};
use clix::{BodyId, Job, JobStatus, Peer, Store};
use ed25519_dalek::{Signer, SigningKey};
use serde::Serialize;
use sha2::{Digest, Sha256};

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
    store.peers = [("origin", 1), ("runner", 2), ("relay", 3)]
        .into_iter()
        .filter(|(n, _)| *n != name)
        .map(|(n, seed)| Peer {
            name: BodyId(n.into()),
            owner_pk: key(seed),
            addr: None,
        })
        .collect();
    // Exercise the production initialization hook before admitting new jobs.
    store.update(|_| Ok(())).unwrap();
    (dir, store)
}

fn sign<T: Serialize>(kind: &str, value: &T, seed: u8) -> Vec<u8> {
    let mut message = b"Clix history v1\0".to_vec();
    message.extend_from_slice(kind.as_bytes());
    message.push(0);
    message.extend(serde_json::to_vec(value).unwrap());
    SigningKey::from_bytes(&[seed; 32])
        .sign(&message)
        .to_bytes()
        .to_vec()
}

fn certificate(legacy_observer: Option<BodyId>, signer: u8) -> Certificate {
    let invocation = Invocation {
        id: ID.into(),
        from: BodyId("origin".into()),
        origin_key: key(1),
        body: BodyId("runner".into()),
        runner_key: key(2),
        argv: vec!["true".into()],
        retry_of: None,
        legacy_observer,
    };
    Certificate {
        signature: sign("invocation", &invocation, signer),
        invocation,
    }
}

fn output_ref(bytes: &[u8]) -> OutputRef {
    OutputRef {
        bytes: bytes.len(),
        sha256: Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    }
}

fn published(
    certificate: Certificate,
    role: Role,
    signer: u8,
    revision: u64,
    status: JobStatus,
    stdout: &[u8],
) -> Published {
    let fact = Fact {
        invocation: history::invocation_hash(&certificate).unwrap(),
        role,
        revision,
        status,
        stdout: output_ref(stdout),
        stderr: output_ref(&[]),
    };
    let signed = SignedFact {
        signature: sign("fact", &fact, signer),
        fact,
    };
    let (origin, runner) = if role == Role::Runner {
        (None, Some(signed))
    } else {
        (Some(signed), None)
    };
    Published {
        record: Record {
            certificate,
            origin,
            runner,
        },
        output: Some(CapturedOutput {
            stdout: stdout.into(),
            stderr: vec![],
        }),
        change: revision,
    }
}

fn control_state(store: &Store) -> serde_json::Value {
    serde_json::to_value((&store.jobs, &store.grants, &store.requests, &store.peers)).unwrap()
}

#[test]
fn a_valid_legacy_observation_cannot_authorize_modern_execution() {
    let (_dir, store) = fixture("runner", 2);
    let origin = store.peers.iter().find(|p| p.name.0 == "origin").unwrap();
    let c = certificate(Some(BodyId("origin".into())), 1);
    history::validate_invocation(&store, &c).unwrap();
    assert!(history::authorize_execution(&store, origin, &c).is_err());
}

#[test]
fn a_valid_modern_signature_is_not_execution_authority_for_a_relay() {
    let (_dir, store) = fixture("runner", 2);
    let c = certificate(None, 1);
    let origin = store.peers.iter().find(|p| p.name.0 == "origin").unwrap();
    let relay = store.peers.iter().find(|p| p.name.0 == "relay").unwrap();
    history::authorize_execution(&store, origin, &c).unwrap();
    assert!(history::authorize_execution(&store, relay, &c).is_err());
}

#[test]
fn legacy_observer_name_does_not_borrow_the_other_participants_signature() {
    let (_dir, store) = fixture("relay", 3);
    let forged = certificate(Some(BodyId("origin".into())), 2);
    assert!(history::validate_invocation(&store, &forged).is_err());
}

#[test]
fn imported_history_changes_no_execution_or_owner_control_records() {
    let (_dir, mut store) = fixture("relay", 3);
    let before = control_state(&store);
    let p = published(
        certificate(None, 1),
        Role::Runner,
        2,
        1,
        JobStatus::Running,
        &[],
    );
    store.update(|s| history::ingest(s, &p)).unwrap();
    assert_eq!(control_state(&store), before);
    assert_eq!(history::projected(&store).len(), 1);
}

#[test]
fn changed_output_is_rejected_without_advancing_history_or_controls() {
    let (_dir, mut store) = fixture("relay", 3);
    let mut p = published(
        certificate(None, 1),
        Role::Runner,
        2,
        1,
        JobStatus::Done { exit: 0 },
        b"real output",
    );
    p.output.as_mut().unwrap().stdout = b"substituted output".to_vec();
    let controls = control_state(&store);
    let history = serde_json::to_value(&store.history).unwrap();
    assert!(store.update(|s| history::ingest(s, &p)).is_err());
    assert_eq!(control_state(&store), controls);
    assert_eq!(serde_json::to_value(&store.history).unwrap(), history);
}

#[test]
fn stale_facts_cannot_regress_completion_and_newer_facts_cannot_rewrite_it() {
    let (_dir, mut store) = fixture("relay", 3);
    let c = certificate(None, 1);
    let done = published(
        c.clone(),
        Role::Runner,
        2,
        2,
        JobStatus::Done { exit: 0 },
        b"done",
    );
    store.update(|s| history::ingest(s, &done)).unwrap();
    let stale = published(c.clone(), Role::Runner, 2, 1, JobStatus::Running, &[]);
    store.update(|s| history::ingest(s, &stale)).unwrap();
    let equal_revision_conflict = published(
        c.clone(),
        Role::Runner,
        2,
        2,
        JobStatus::Done { exit: 1 },
        b"done",
    );
    assert!(store
        .update(|s| history::ingest(s, &equal_revision_conflict))
        .is_err());
    let rewrite = published(
        c,
        Role::Runner,
        2,
        3,
        JobStatus::Failed {
            reason: "replacement".into(),
        },
        &[],
    );
    assert!(store.update(|s| history::ingest(s, &rewrite)).is_err());
    let projected = history::projected(&store);
    assert_eq!(projected[0].status, JobStatus::Done { exit: 0 });
    assert_eq!(projected[0].stdout, b"done");
}

#[test]
fn runner_output_survives_an_origins_failed_local_delivery_record() {
    let (_dir, mut store) = fixture("origin", 1);
    let job = Job {
        id: ID.into(),
        from: BodyId("origin".into()),
        body: BodyId("runner".into()),
        argv: vec!["true".into()],
        status: JobStatus::Failed {
            reason: "delivery observation".into(),
        },
        stdout: vec![],
        stderr: vec![],
    };
    // The production store hook mints and binds the locally admitted certificate.
    store
        .update(|s| {
            s.jobs.push(job.clone());
            Ok(())
        })
        .unwrap();
    let certificate = history::for_job(&store, &job)
        .unwrap()
        .record
        .certificate
        .clone();
    let p = published(
        certificate,
        Role::Runner,
        2,
        1,
        JobStatus::Done { exit: 0 },
        b"runner result\0\xff",
    );
    store.update(|s| history::ingest(s, &p)).unwrap();
    let entry = history::for_job(&store, &job).unwrap();
    let exported = history::export_entry(&store, entry);
    assert_eq!(exported.output.unwrap().stdout, b"runner result\0\xff");
    let projected = history::projected(&store);
    assert_eq!(projected[0].status, JobStatus::Done { exit: 0 });
    assert_eq!(projected[0].stdout, b"runner result\0\xff");
    assert!(matches!(store.jobs[0].status, JobStatus::Failed { .. }));
}

#[test]
fn imported_certificate_cannot_modernize_a_legacy_admission() {
    let (dir, mut store) = fixture("runner", 2);
    let job = Job {
        id: ID.into(),
        from: BodyId("origin".into()),
        body: BodyId("runner".into()),
        argv: vec!["true".into()],
        status: JobStatus::Done { exit: 0 },
        stdout: b"old result".to_vec(),
        stderr: vec![],
    };
    store.history = Default::default();
    store.jobs.push(job.clone());
    store.save().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.recover_jobs().unwrap();
    let binding = store.job_certificates[ID].clone();
    assert!(store.legacy_jobs.contains(ID));
    let modern = published(
        certificate(None, 1),
        Role::Origin,
        1,
        1,
        JobStatus::Queued,
        &[],
    );
    store.update(|s| history::ingest(s, &modern)).unwrap();
    store.update(|_| Ok(())).unwrap();
    assert_eq!(store.job_certificates[ID], binding);
    let modern_key = history::invocation_hash(&modern.record.certificate).unwrap();
    assert!(store.history.entries[&modern_key].record.runner.is_none());
    assert!(store.history.entries[&binding]
        .record
        .certificate
        .invocation
        .legacy_observer
        .is_some());
}

#[test]
fn later_origin_delivery_updates_do_not_drop_a_selected_runner_payload() {
    let (_dir, mut store) = fixture("origin", 1);
    let job = Job {
        id: ID.into(),
        from: BodyId("origin".into()),
        body: BodyId("runner".into()),
        argv: vec!["true".into()],
        status: JobStatus::Queued,
        stdout: vec![],
        stderr: vec![],
    };
    store
        .update(|s| {
            s.jobs.push(job.clone());
            Ok(())
        })
        .unwrap();
    let certificate = history::for_job(&store, &job)
        .unwrap()
        .record
        .certificate
        .clone();
    let p = published(
        certificate,
        Role::Runner,
        2,
        1,
        JobStatus::Done { exit: 0 },
        b"completed elsewhere",
    );
    store.update(|s| history::ingest(s, &p)).unwrap();
    for status in [
        JobStatus::WaitingBody,
        JobStatus::Failed {
            reason: "delivery unavailable".into(),
        },
    ] {
        store
            .update(|s| {
                s.jobs[0].status = status;
                Ok(())
            })
            .unwrap();
        let entry = history::for_job(&store, &store.jobs[0]).unwrap();
        assert_eq!(
            history::export_entry(&store, entry).output.unwrap().stdout,
            b"completed elsewhere"
        );
        assert_eq!(history::projected(&store)[0].stdout, b"completed elsewhere");
    }
}
