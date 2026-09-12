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

fn numbered_certificate(number: usize) -> Certificate {
    let mut c = certificate(None, 1);
    c.invocation.id = format!("00000000-0000-4000-8000-{number:012}");
    c.signature = sign("invocation", &c.invocation, 1);
    c
}

fn merge_page(store: &mut Store, page: &history::Page) {
    // Exercise durable merge/cursor publication through the same Store.update
    // boundary as production; this helper does not test TLS/page validation.
    store
        .update(|s| {
            for item in &page.entries {
                history::ingest(s, item)?;
            }
            s.history.peers.insert(
                "runner".into(),
                history::Progress {
                    epoch: page.epoch.clone(),
                    cursor: page.next,
                    high: page.high,
                    complete: page.complete,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn captured_high_water_replacements_are_seen_in_the_following_scan() {
    let (_dir, mut publisher) = fixture("relay", 3);
    for n in 1..=20 {
        let p = published(
            numbered_certificate(n),
            Role::Runner,
            2,
            1,
            JobStatus::Running,
            &[],
        );
        publisher.update(|s| history::ingest(s, &p)).unwrap();
    }
    let first = history::page(&publisher, "", 0, None).unwrap();
    assert!(first.reset);
    assert!(!first.complete);
    assert_eq!((first.entries.len(), first.next, first.high), (16, 16, 20));
    // One row was already delivered; one had not yet been delivered.
    for n in [1, 17] {
        let p = published(
            numbered_certificate(n),
            Role::Runner,
            2,
            2,
            JobStatus::Done { exit: 0 },
            b"replacement",
        );
        publisher.update(|s| history::ingest(s, &p)).unwrap();
    }
    let tail = history::page(&publisher, &first.epoch, first.next, Some(first.high)).unwrap();
    assert!(tail.complete);
    assert_eq!(tail.next, 20);
    assert_eq!(tail.entries.len(), 3);
    assert!(tail.entries.iter().all(|p| p.change <= first.high));
    let replacements = history::page(&publisher, &first.epoch, tail.next, None).unwrap();
    assert!(replacements.complete);
    assert_eq!(replacements.high, 22);
    assert_eq!(replacements.entries.len(), 2);
    let ids: Vec<_> = replacements
        .entries
        .iter()
        .map(|p| p.record.certificate.invocation.id.clone())
        .collect();
    assert_eq!(
        ids,
        vec![
            numbered_certificate(1).invocation.id,
            numbered_certificate(17).invocation.id
        ]
    );
    let (_receiver_dir, mut receiver) = fixture("origin", 1);
    for page in [&first, &tail, &replacements] {
        merge_page(&mut receiver, page);
    }
    assert_eq!(receiver.history.entries.len(), 20);
    for n in [1, 17] {
        let k = history::invocation_hash(&numbered_certificate(n)).unwrap();
        assert_eq!(
            receiver.history.entries[&k]
                .record
                .selected()
                .unwrap()
                .fact
                .status,
            JobStatus::Done { exit: 0 }
        );
        assert_eq!(
            receiver.history.entries[&k].output.as_ref().unwrap().stdout,
            b"replacement"
        );
    }
    assert!(receiver.jobs.is_empty());
}

#[test]
fn changed_epoch_or_future_cursor_resets_scan_without_an_old_high_water() {
    let (_dir, mut store) = fixture("relay", 3);
    for n in 1..=3 {
        let p = published(
            numbered_certificate(n),
            Role::Runner,
            2,
            1,
            JobStatus::Running,
            &[],
        );
        store.update(|s| history::ingest(s, &p)).unwrap();
    }
    let first = history::page(&store, "", 0, None).unwrap();
    store
        .update(|s| {
            s.history.epoch = "00000000-0000-4000-8000-999999999999".into();
            Ok(())
        })
        .unwrap();
    let reset = history::page(&store, &first.epoch, first.next, Some(1)).unwrap();
    assert!(reset.reset);
    assert!(reset.complete);
    assert_eq!(reset.entries.len(), 3);
    assert_eq!((reset.high, reset.next), (3, 3));
    assert_ne!(reset.epoch, first.epoch);
    let future = history::page(&store, &reset.epoch, 100, Some(1)).unwrap();
    assert!(future.reset);
    assert_eq!(future.entries.len(), 3);
    assert_eq!(future.next, 3);
    assert!(history::page(&store, &reset.epoch, 2, Some(1)).is_err());
}

#[test]
fn replaying_a_page_after_durable_merge_and_restart_is_idempotent() {
    let (_publisher_dir, mut publisher) = fixture("relay", 3);
    let p = published(
        certificate(None, 1),
        Role::Runner,
        2,
        1,
        JobStatus::Done { exit: 7 },
        b"durable\0\xff",
    );
    publisher.update(|s| history::ingest(s, &p)).unwrap();
    let page = history::page(&publisher, "", 0, None).unwrap();
    let (receiver_dir, mut receiver) = fixture("origin", 1);
    let controls = control_state(&receiver);
    merge_page(&mut receiver, &page);
    let saved = serde_json::to_value(&receiver.history).unwrap();
    drop(receiver);
    let mut receiver = Store::open(receiver_dir.path()).unwrap();
    assert_eq!(serde_json::to_value(&receiver.history).unwrap(), saved);
    merge_page(&mut receiver, &page);
    assert_eq!(serde_json::to_value(&receiver.history).unwrap(), saved);
    assert_eq!(control_state(&receiver), controls);
    let view = history::views(&receiver).remove(0);
    assert_eq!(view.job.stdout, b"durable\0\xff");
    assert!(view.output_available);
    assert_eq!(receiver.history.peers["runner"].cursor, page.next);
}

#[test]
fn legacy_runner_completion_keeps_all_provenance_in_every_arrival_order() {
    let legacy_origin = published(
        certificate(Some(BodyId("origin".into())), 1),
        Role::LegacyObserver,
        1,
        1,
        JobStatus::Failed {
            reason: "origin lost contact".into(),
        },
        &[],
    );
    let legacy_runner = published(
        certificate(Some(BodyId("runner".into())), 2),
        Role::LegacyObserver,
        2,
        1,
        JobStatus::Done { exit: 0 },
        b"legacy runner result",
    );
    let modern_origin = published(
        certificate(None, 1),
        Role::Origin,
        1,
        1,
        JobStatus::WaitingBody,
        &[],
    );
    let items = [legacy_origin, legacy_runner, modern_origin];
    let mut expected = None;
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let (_dir, mut store) = fixture("relay", 3);
        for n in order {
            store.update(|s| history::ingest(s, &items[n])).unwrap();
        }
        let views = history::views(&store);
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!(view.job.status, JobStatus::Done { exit: 0 });
        assert_eq!(view.job.stdout, b"legacy runner result");
        assert!(view.output_available);
        assert_eq!(view.provenance, "legacy record observed by runner");
        assert_eq!(view.observations.len(), 3);
        assert!(view
            .observations
            .iter()
            .any(|o| o.role == Role::Origin && o.legacy_observer.is_none()));
        assert!(view
            .observations
            .iter()
            .any(|o| o.legacy_observer == Some(BodyId("origin".into()))));
        assert!(view
            .observations
            .iter()
            .any(|o| o.legacy_observer == Some(BodyId("runner".into()))));
        let value = serde_json::to_value(views).unwrap();
        if let Some(expected) = &expected {
            assert_eq!(&value, expected);
        } else {
            expected = Some(value);
        }
    }
}

#[test]
fn changed_certificate_identity_argv_and_retry_link_fail_closed() {
    let (_dir, mut store) = fixture("relay", 3);
    let good = published(
        certificate(None, 1),
        Role::Runner,
        2,
        1,
        JobStatus::Done { exit: 0 },
        b"signed result",
    );
    let mut variants = Vec::new();
    for field in 0..7 {
        let mut p = good.clone();
        let i = &mut p.record.certificate.invocation;
        match field {
            0 => i.origin_key = key(3),
            1 => i.runner_key = key(3),
            2 => i.argv.push("changed".into()),
            3 => i.retry_of = Some("00000000-0000-4000-8000-000000000002".into()),
            4 => i.from = BodyId("relay".into()),
            5 => i.body = BodyId("relay".into()),
            6 => i.id = "00000000-0000-4000-8000-000000000002".into(),
            _ => unreachable!(),
        }
        variants.push(p);
    }
    // Even a newly valid origin certificate cannot borrow an earlier runner fact.
    for field in 0..2 {
        let mut p = good.clone();
        let c = &mut p.record.certificate;
        if field == 0 {
            c.invocation.argv.push("new invocation".into());
        } else {
            c.invocation.retry_of = Some("00000000-0000-4000-8000-000000000002".into());
        }
        c.signature = sign("invocation", &c.invocation, 1);
        history::validate_invocation(&store, c).unwrap();
        variants.push(p);
    }
    let controls = control_state(&store);
    let before = serde_json::to_value(&store.history).unwrap();
    for (index, p) in variants.iter().enumerate() {
        assert!(
            store.update(|s| history::ingest(s, p)).is_err(),
            "accepted tamper case {index}"
        );
        assert_eq!(control_state(&store), controls);
        assert_eq!(serde_json::to_value(&store.history).unwrap(), before);
    }
}

#[test]
fn wrong_fact_role_signer_and_slot_are_rejected_atomically() {
    let (_dir, mut store) = fixture("relay", 3);
    let c = certificate(None, 1);
    let mut variants = vec![
        published(
            c.clone(),
            Role::Origin,
            1,
            1,
            JobStatus::Done { exit: 0 },
            &[],
        ),
        published(
            c.clone(),
            Role::Runner,
            1,
            1,
            JobStatus::Done { exit: 0 },
            &[],
        ),
        published(c.clone(), Role::Origin, 2, 1, JobStatus::Queued, &[]),
        published(
            c.clone(),
            Role::LegacyObserver,
            1,
            1,
            JobStatus::Queued,
            &[],
        ),
        published(c.clone(), Role::Runner, 2, 1, JobStatus::WaitingBody, &[]),
        published(c.clone(), Role::Runner, 2, 0, JobStatus::Running, &[]),
    ];
    let mut slot = published(c, Role::Runner, 2, 1, JobStatus::Done { exit: 0 }, &[]);
    slot.record.origin = slot.record.runner.take();
    variants.push(slot);
    variants.push(published(
        certificate(Some(BodyId("runner".into())), 2),
        Role::Runner,
        2,
        1,
        JobStatus::Done { exit: 0 },
        &[],
    ));
    let controls = control_state(&store);
    let before = serde_json::to_value(&store.history).unwrap();
    for (index, p) in variants.iter().enumerate() {
        assert!(
            store.update(|s| history::ingest(s, p)).is_err(),
            "accepted role case {index}"
        );
        assert_eq!(control_state(&store), controls);
        assert_eq!(serde_json::to_value(&store.history).unwrap(), before);
    }
}

#[test]
fn migration_without_a_historical_peer_preserves_unknown_identity_and_legacy_binding() {
    let (dir, mut store) = fixture("runner", 2);
    store.peers.retain(|p| p.name.0 != "origin");
    store.jobs.push(Job {
        id: ID.into(),
        from: BodyId("origin".into()),
        body: BodyId("runner".into()),
        argv: vec!["true".into()],
        status: JobStatus::Done { exit: 0 },
        stdout: b"before upgrade".to_vec(),
        stderr: vec![],
    });
    let before = control_state(&store);
    let mut old = serde_json::to_value(&store).unwrap();
    for field in [
        "history",
        "retry_links",
        "job_certificates",
        "legacy_jobs",
        "output_pruned",
        "format_version",
    ] {
        old.as_object_mut().unwrap().remove(field);
    }
    std::fs::write(
        dir.path().join("state.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    drop(store);
    let mut store = Store::open(dir.path()).unwrap();
    store.recover_jobs().unwrap();
    assert_eq!(control_state(&store), before);
    assert!(store.legacy_jobs.contains(ID));
    let binding = store.job_certificates[ID].clone();
    let cert = store.history.entries[&binding].record.certificate.clone();
    assert_eq!(
        cert.invocation.legacy_observer,
        Some(BodyId("runner".into()))
    );
    assert!(cert.invocation.origin_key.is_empty());
    assert_eq!(cert.invocation.runner_key, key(2));
    history::validate_invocation(&store, &cert).unwrap();
    let view = history::views(&store).remove(0);
    assert_eq!(view.job.stdout, b"before upgrade");
    assert_eq!(view.provenance, "legacy record observed by runner");
    store
        .update(|s| {
            s.peers.push(Peer {
                name: BodyId("origin".into()),
                owner_pk: key(1),
                addr: None,
            });
            Ok(())
        })
        .unwrap();
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
    assert_eq!(store.history.entries[&binding].record.certificate, cert);
    let modern_key = history::invocation_hash(&modern.record.certificate).unwrap();
    assert!(store.history.entries[&modern_key].record.runner.is_none());
    assert_eq!(store.jobs[0].stdout, b"before upgrade");
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert!(store.legacy_jobs.contains(ID));
    assert_eq!(store.job_certificates[ID], binding);
}

#[test]
fn pairing_a_previously_unknown_author_resets_durable_scan_progress() {
    let (_dir, mut store) = fixture("relay", 3);
    store
        .update(|s| {
            s.peers.retain(|p| p.name.0 != "origin");
            Ok(())
        })
        .unwrap();
    let item = published(
        certificate(None, 1),
        Role::Runner,
        2,
        1,
        JobStatus::Done { exit: 0 },
        b"unknown origin",
    );
    assert!(store.update(|s| history::ingest(s, &item)).is_err());
    store
        .update(|s| {
            s.history.peers.insert(
                "runner".into(),
                history::Progress {
                    epoch: "old epoch".into(),
                    cursor: 23,
                    high: 23,
                    complete: true,
                    unknown_authors: true,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .unwrap();
    assert!(store.history.peers["runner"].unknown_authors);
    store
        .update(|s| {
            s.peers.push(Peer {
                name: BodyId("origin".into()),
                owner_pk: key(1),
                addr: None,
            });
            Ok(())
        })
        .unwrap();
    assert!(store.history.peers.is_empty());
    store.update(|s| history::ingest(s, &item)).unwrap();
    assert!(store.jobs.is_empty());
    assert_eq!(history::projected(&store)[0].stdout, b"unknown origin");
}

#[test]
fn display_order_follows_local_observations_without_treating_ids_as_time() {
    let (_dir, mut store) = fixture("origin", 1);
    let first = "ffffffff-ffff-4fff-8fff-ffffffffffff";
    let second = "00000000-0000-4000-8000-000000000001";
    for id in [first, second] {
        store
            .update(|s| {
                s.jobs.push(Job {
                    id: id.into(),
                    from: BodyId("origin".into()),
                    body: BodyId("runner".into()),
                    argv: vec!["true".into()],
                    status: JobStatus::Queued,
                    stdout: vec![],
                    stderr: vec![],
                });
                Ok(())
            })
            .unwrap();
    }
    let ids = |store: &Store| {
        history::views(store)
            .into_iter()
            .map(|v| v.job.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&store), vec![first, second]);
    let c = history::for_job(&store, &store.jobs[0])
        .unwrap()
        .record
        .certificate
        .clone();
    let result = published(
        c,
        Role::Runner,
        2,
        1,
        JobStatus::Done { exit: 0 },
        b"observed later",
    );
    store.update(|s| history::ingest(s, &result)).unwrap();
    assert_eq!(ids(&store), vec![second, first]);
    assert_eq!(
        history::views(&store)[1].provenance,
        "verified runner result"
    );
    assert_eq!(
        store.jobs[0].status,
        JobStatus::Queued,
        "display does not authorize or settle local delivery"
    );
}
