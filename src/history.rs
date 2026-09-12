//! Verified history is a read-only replica, never an execution queue.
//!
//! The origin owns the invocation and delivery observations; the runner owns
//! acceptance and outcome. Signatures survive relaying. Only explicit pairing
//! supplies trusted keys. Old records are labelled observations, not retroactive
//! proof that another machine signed an invocation or a result.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{ClixError, Result};
use crate::store::Store;
use crate::types::{BodyId, Job, JobStatus, Peer};

const DOMAIN: &[u8] = b"Clix history v1\0";
const PAGE_ROWS: usize = 16;
const PAGE_BYTES: usize = 48 * 1024 * 1024;
const STREAM_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Invocation {
    pub id: String,
    pub from: BodyId,
    pub origin_key: Vec<u8>,
    pub body: BodyId,
    pub runner_key: Vec<u8>,
    pub argv: Vec<String>,
    pub retry_of: Option<String>,
    /// The named machine observes its own pre-upgrade record. It does not
    /// authenticate the other participant's historical decision.
    pub legacy_observer: Option<BodyId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Certificate {
    pub invocation: Invocation,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Origin,
    Runner,
    LegacyObserver,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputRef {
    pub bytes: usize,
    pub sha256: String,
}

impl OutputRef {
    fn of(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.len(),
            sha256: hash(bytes),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fact {
    pub invocation: String,
    pub role: Role,
    pub revision: u64,
    pub status: JobStatus,
    pub stdout: OutputRef,
    pub stderr: OutputRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedFact {
    pub fact: Fact,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    pub certificate: Certificate,
    pub origin: Option<SignedFact>,
    pub runner: Option<SignedFact>,
}

impl Record {
    pub fn selected(&self) -> Option<&SignedFact> {
        self.runner.as_ref().or(self.origin.as_ref())
    }

    fn key(&self) -> Result<String> {
        invocation_hash(&self.certificate)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapturedOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub record: Record,
    pub change: u64,
    /// Locally admitted jobs already own their output in Store.jobs. Only
    /// replicas need another payload. Hashes remain after explicit pruning.
    pub output: Option<CapturedOutput>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Progress {
    pub epoch: String,
    pub cursor: u64,
    pub high: u64,
    pub complete: bool,
    #[serde(default)]
    pub unknown_authors: bool,
    #[serde(skip)]
    pub last_contact: Option<u64>,
    #[serde(skip)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct History {
    pub initialized: bool,
    pub epoch: String,
    pub clock: u64,
    pub trust: String,
    pub entries: BTreeMap<String, Entry>,
    pub peers: BTreeMap<String, Progress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Published {
    pub record: Record,
    pub output: Option<CapturedOutput>,
    pub change: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub epoch: String,
    pub reset: bool,
    pub high: u64,
    pub next: u64,
    pub complete: bool,
    pub entries: Vec<Published>,
}

fn protocol(message: &str) -> ClixError {
    ClixError::Protocol(message.into())
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn bytes<T: Serialize>(kind: &str, value: &T) -> Result<Vec<u8>> {
    let mut bytes = DOMAIN.to_vec();
    bytes.extend_from_slice(kind.as_bytes());
    bytes.push(0);
    bytes.extend(serde_json::to_vec(value)?);
    Ok(bytes)
}

fn signing_key(store: &Store) -> Result<SigningKey> {
    let key = store
        .owner_sk
        .as_slice()
        .try_into()
        .map_err(|_| protocol("invalid local history identity"))?;
    Ok(SigningKey::from_bytes(key))
}

pub fn public_key(store: &Store) -> Result<Vec<u8>> {
    Ok(signing_key(store)?.verifying_key().to_bytes().to_vec())
}

fn known_key(store: &Store, name: &BodyId) -> Result<Vec<u8>> {
    if name.0 == store.body_name {
        return public_key(store);
    }
    store
        .peers
        .iter()
        .find(|p| p.name == *name)
        .map(|p| p.owner_pk.clone())
        .ok_or_else(|| protocol("history includes an author that is not explicitly paired"))
}

fn verify<T: Serialize>(kind: &str, value: &T, sig: &[u8], key: &[u8]) -> Result<()> {
    let key = VerifyingKey::from_bytes(
        key.try_into()
            .map_err(|_| protocol("invalid history public key"))?,
    )
    .map_err(|_| protocol("invalid history public key"))?;
    let signature =
        Signature::from_slice(sig).map_err(|_| protocol("invalid history signature length"))?;
    key.verify_strict(&bytes(kind, value)?, &signature)
        .map_err(|_| protocol("history signature did not verify"))
}

pub fn invocation_hash(certificate: &Certificate) -> Result<String> {
    Ok(hash(&bytes("invocation", &certificate.invocation)?))
}

pub fn validate_invocation(store: &Store, c: &Certificate) -> Result<()> {
    let i = &c.invocation;
    crate::limits::invocation(&i.id, &i.argv)?;
    if let Some(id) = &i.retry_of {
        crate::limits::identifier(id)?;
        if id == &i.id {
            return Err(protocol("a retry must have a new job ID"));
        }
    }
    let signer = match &i.legacy_observer {
        Some(observer) if observer == &i.from || observer == &i.body => {
            let key = known_key(store, observer)?;
            let recorded = if observer == &i.from {
                &i.origin_key
            } else {
                &i.runner_key
            };
            if recorded != &key {
                return Err(protocol(
                    "legacy observer identity differs from its paired key",
                ));
            }
            key
        }
        Some(_) => return Err(protocol("legacy observer was not a job participant")),
        None => {
            if known_key(store, &i.from)? != i.origin_key
                || known_key(store, &i.body)? != i.runner_key
            {
                return Err(protocol("history identity differs from the paired key"));
            }
            i.origin_key.clone()
        }
    };
    verify("invocation", i, &c.signature, &signer)
}

pub fn authorize_execution(store: &Store, peer: &Peer, c: &Certificate) -> Result<()> {
    validate_invocation(store, c)?;
    let i = &c.invocation;
    if i.legacy_observer.is_some()
        || i.from != peer.name
        || i.origin_key != peer.owner_pk
        || i.body.0 != store.body_name
        || i.runner_key != public_key(store)?
    {
        return Err(protocol("history relay is not execution authority"));
    }
    Ok(())
}

/// Only execution admission may bind a certificate to a local job. A history
/// import, even with a valid origin signature, never calls this function.
pub(crate) fn bind_admission(store: &mut Store, c: &Certificate) -> Result<()> {
    let key = invocation_hash(c)?;
    let id = &c.invocation.id;
    if let Some(old) = store.job_certificates.get(id) {
        if old != &key {
            return Err(protocol(
                "job ID is already bound to a different invocation",
            ));
        }
    } else if store.jobs.iter().any(|j| &j.id == id) {
        return Err(protocol("job ID belongs to an earlier unsigned admission"));
    }
    if !store.history.entries.contains_key(&key) {
        let change = next_change(&mut store.history)?;
        store.history.entries.insert(
            key.clone(),
            Entry {
                record: Record {
                    certificate: c.clone(),
                    origin: None,
                    runner: None,
                },
                change,
                output: None,
            },
        );
    }
    store.job_certificates.insert(id.clone(), key);
    if let Some(old) = &c.invocation.retry_of {
        store.retry_links.insert(id.clone(), old.clone());
    }
    Ok(())
}

pub(crate) fn execution_reply(store: &Store, c: &Certificate) -> Result<Value> {
    let key = invocation_hash(c)?;
    let job = store.jobs.iter().find(|j| j.id == c.invocation.id);
    let Some(job) = job else {
        return Ok(json!({"job":null}));
    };
    if !matches_job(c, job) || store.job_certificates.get(&job.id) != Some(&key) {
        return Err(protocol(
            "job ID is bound to a different admitted invocation",
        ));
    }
    let entry = store
        .history
        .entries
        .get(&key)
        .ok_or_else(|| protocol("admission is missing history"))?;
    Ok(json!({"job":job,"history":export_entry(store,entry)}))
}

pub(crate) fn accept_runner_reply(
    store: &mut Store,
    peer: &BodyId,
    published: &Published,
) -> Result<Option<Job>> {
    validate_record(store, &published.record)?;
    let c = &published.record.certificate;
    let key = invocation_hash(c)?;
    if c.invocation.legacy_observer.is_some() || c.invocation.body != *peer {
        return Err(protocol("response has no modern runner authority"));
    }
    let signed = published
        .record
        .runner
        .as_ref()
        .ok_or_else(|| protocol("runner response is missing its signed result"))?;
    let Some(index) = store.jobs.iter().position(|j| j.id == c.invocation.id) else {
        return Ok(None);
    };
    if store.job_certificates.get(&c.invocation.id) != Some(&key)
        || !matches_job(c, &store.jobs[index])
    {
        return Err(protocol("runner result belongs to a different admission"));
    }
    ingest(store, published)?;
    // An earlier delivery error remains an origin observation. Replicated
    // runner facts can settle the displayed result without restarting delivery.
    if crate::job::is_terminal(&store.jobs[index].status) {
        return Ok(None);
    }
    let locally_pruned =
        store.history_pruned.contains(&key) || store.output_pruned.contains(&c.invocation.id);
    let retained = published.output.as_ref().filter(|_| !locally_pruned);
    let output = retained.cloned().unwrap_or(CapturedOutput {
        stdout: Vec::new(),
        stderr: Vec::new(),
    });
    if retained.is_none() && (signed.fact.stdout.bytes != 0 || signed.fact.stderr.bytes != 0) {
        store.output_pruned.insert(c.invocation.id.clone());
    }
    let incoming = Job {
        id: c.invocation.id.clone(),
        from: c.invocation.from.clone(),
        body: c.invocation.body.clone(),
        argv: c.invocation.argv.clone(),
        status: signed.fact.status.clone(),
        stdout: output.stdout,
        stderr: output.stderr,
    };
    store.jobs[index] = incoming.clone();
    Ok(Some(incoming))
}

fn validate_fact(store: &Store, c: &Certificate, signed: &SignedFact, role: Role) -> Result<()> {
    let f = &signed.fact;
    if f.role != role || f.invocation != invocation_hash(c)? || f.revision == 0 {
        return Err(protocol(
            "history fact is bound to a different invocation or role",
        ));
    }
    for output in [&f.stdout, &f.stderr] {
        if output.bytes > STREAM_BYTES
            || output.sha256.len() != 64
            || !output.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(protocol("invalid history output digest or size"));
        }
    }
    let i = &c.invocation;
    let key = match role {
        Role::Origin => {
            if i.legacy_observer.is_some()
                || !matches!(
                    f.status,
                    JobStatus::Queued
                        | JobStatus::WaitingBody
                        | JobStatus::WaitingCapacity
                        | JobStatus::Failed { .. }
                )
                || f.stdout.bytes != 0
                || f.stderr.bytes != 0
            {
                return Err(protocol("origin cannot publish a runner outcome"));
            }
            i.origin_key.clone()
        }
        Role::Runner => {
            if i.legacy_observer.is_some()
                || matches!(
                    f.status,
                    JobStatus::Queued | JobStatus::WaitingBody | JobStatus::WaitingCapacity
                )
            {
                return Err(protocol("invalid runner fact"));
            }
            i.runner_key.clone()
        }
        Role::LegacyObserver => known_key(
            store,
            i.legacy_observer
                .as_ref()
                .ok_or_else(|| protocol("missing legacy observer"))?,
        )?,
    };
    crate::limits::status(&f.status)?;
    verify("fact", f, &signed.signature, &key)
}

fn validate_record(store: &Store, record: &Record) -> Result<()> {
    validate_invocation(store, &record.certificate)?;
    if record.certificate.invocation.legacy_observer.is_some() {
        if record.runner.is_some() {
            return Err(protocol(
                "legacy history is an observation, not a runner signature",
            ));
        }
        if let Some(fact) = &record.origin {
            validate_fact(store, &record.certificate, fact, Role::LegacyObserver)?;
        }
    } else {
        if let Some(fact) = &record.origin {
            validate_fact(store, &record.certificate, fact, Role::Origin)?;
        }
        if let Some(fact) = &record.runner {
            validate_fact(store, &record.certificate, fact, Role::Runner)?;
        }
    }
    Ok(())
}

fn matches_job(c: &Certificate, j: &Job) -> bool {
    let i = &c.invocation;
    i.id == j.id && i.from == j.from && i.body == j.body && i.argv == j.argv
}

fn certificate(store: &Store, job: &Job, legacy: bool) -> Result<Certificate> {
    let invocation = Invocation {
        id: job.id.clone(),
        from: job.from.clone(),
        origin_key: if legacy {
            known_key(store, &job.from).unwrap_or_default()
        } else {
            known_key(store, &job.from)?
        },
        body: job.body.clone(),
        runner_key: if legacy {
            known_key(store, &job.body).unwrap_or_default()
        } else {
            known_key(store, &job.body)?
        },
        argv: job.argv.clone(),
        retry_of: store.retry_links.get(&job.id).cloned(),
        legacy_observer: legacy.then(|| BodyId(store.body_name.clone())),
    };
    let signature = signing_key(store)?
        .sign(&bytes("invocation", &invocation)?)
        .to_bytes()
        .to_vec();
    Ok(Certificate {
        invocation,
        signature,
    })
}

fn next_change(history: &mut History) -> Result<u64> {
    history.clock = history
        .clock
        .checked_add(1)
        .ok_or_else(|| protocol("history sequence exhausted"))?;
    Ok(history.clock)
}

/// Called inside the same durable update as admission or execution transitions.
/// It publishes only locally owned facts for jobs already admitted on this box.
pub fn refresh_owned(store: &mut Store, previous: Option<&Store>) -> Result<()> {
    let legacy = !store.history.initialized;
    if legacy {
        store
            .legacy_jobs
            .extend(store.jobs.iter().map(|j| j.id.clone()));
    }
    if store.history.epoch.is_empty() {
        store.history.epoch = crate::job::new_id()?;
    }
    let mut trust: Vec<_> = store
        .peers
        .iter()
        .map(|p| (&p.name.0, &p.owner_pk))
        .collect();
    trust.sort_by(|a, b| a.0.cmp(b.0));
    let trust = hash(&serde_json::to_vec(&trust)?);
    if trust != store.history.trust {
        // A formerly unknown author may now be explicitly paired. Rescan every
        // neighbor instead of silently losing facts skipped on an earlier pass.
        store.history.peers.clear();
        store.history.trust = trust;
    }
    let old: BTreeMap<_, _> = previous
        .into_iter()
        .flat_map(|s| s.jobs.iter())
        .map(|j| (&j.id, j))
        .collect();
    let changed_jobs: Vec<_> = store
        .jobs
        .iter()
        .filter(|j| legacy || old.get(&j.id).is_none_or(|old| *old != *j))
        .cloned()
        .collect();
    for job in changed_jobs {
        if job.from.0 != store.body_name && job.body.0 != store.body_name {
            continue;
        }
        let key = match store.job_certificates.get(&job.id).cloned() {
            Some(key) => key,
            None => {
                let legacy = store.legacy_jobs.contains(&job.id)
                    || job.from.0 != store.body_name
                    || old.contains_key(&job.id);
                if legacy {
                    store.legacy_jobs.insert(job.id.clone());
                }
                let c = certificate(store, &job, legacy)?;
                let key = invocation_hash(&c)?;
                let change = next_change(&mut store.history)?;
                store.history.entries.insert(
                    key.clone(),
                    Entry {
                        record: Record {
                            certificate: c,
                            origin: None,
                            runner: None,
                        },
                        change,
                        output: None,
                    },
                );
                store.job_certificates.insert(job.id.clone(), key.clone());
                key
            }
        };
        let record = &store
            .history
            .entries
            .get(&key)
            .ok_or_else(|| protocol("admitted job lost its certificate"))?
            .record;
        if !matches_job(&record.certificate, &job) {
            return Err(protocol("admitted job invocation changed"));
        }
        let (role, previous, status) = if record.certificate.invocation.legacy_observer.is_some() {
            (
                Role::LegacyObserver,
                record.origin.as_ref(),
                job.status.clone(),
            )
        } else if job.body.0 == store.body_name
            && !matches!(job.status, JobStatus::Queued | JobStatus::WaitingCapacity)
        {
            (Role::Runner, record.runner.as_ref(), job.status.clone())
        } else if job.from.0 == store.body_name {
            let status = match &job.status {
                JobStatus::Queued
                | JobStatus::WaitingBody
                | JobStatus::WaitingCapacity
                | JobStatus::Failed { .. } => job.status.clone(),
                _ => continue,
            };
            (Role::Origin, record.origin.as_ref(), status)
        } else {
            continue;
        };
        let (stdout, stderr) = if role == Role::Origin {
            (OutputRef::of(&[]), OutputRef::of(&[]))
        } else if store.output_pruned.contains(&job.id) {
            match previous {
                Some(f) => (f.fact.stdout.clone(), f.fact.stderr.clone()),
                None => return Err(protocol("pruned job is missing its output digests")),
            }
        } else {
            (OutputRef::of(&job.stdout), OutputRef::of(&job.stderr))
        };
        if previous.is_some_and(|p| {
            p.fact.status == status && p.fact.stdout == stdout && p.fact.stderr == stderr
        }) {
            continue;
        }
        if previous.is_some_and(|p| crate::job::is_terminal(&p.fact.status)) {
            // Origin observations stay immutable even when a later verified
            // runner result becomes the selected outcome.
            continue;
        }
        let revision = previous.map_or(Ok(1), |p| {
            p.fact
                .revision
                .checked_add(1)
                .ok_or_else(|| protocol("fact revision exhausted"))
        })?;
        let fact = Fact {
            invocation: key.clone(),
            role,
            revision,
            status,
            stdout,
            stderr,
        };
        let signed = SignedFact {
            signature: signing_key(store)?
                .sign(&bytes("fact", &fact)?)
                .to_bytes()
                .to_vec(),
            fact,
        };
        let change = next_change(&mut store.history)?;
        let entry = store.history.entries.get_mut(&key).unwrap();
        if role == Role::Runner {
            entry.record.runner = Some(signed);
        } else {
            entry.record.origin = Some(signed);
        }
        entry.change = change;
        if let Some(selected) = entry.record.selected() {
            let local_has_output = !store.output_pruned.contains(&job.id)
                && OutputRef::of(&job.stdout) == selected.fact.stdout
                && OutputRef::of(&job.stderr) == selected.fact.stderr;
            if local_has_output
                || entry
                    .output
                    .as_ref()
                    .is_some_and(|o| !output_matches(o, &selected.fact))
            {
                entry.output = None;
            }
        }
    }
    store.history.initialized = true;
    Ok(())
}

fn merge_fact(existing: &mut Option<SignedFact>, incoming: &Option<SignedFact>) -> Result<bool> {
    let Some(incoming) = incoming else {
        return Ok(false);
    };
    if let Some(current) = existing {
        if incoming.fact.revision < current.fact.revision {
            return Ok(false);
        }
        if incoming.fact.revision == current.fact.revision {
            if incoming != current {
                return Err(protocol("conflicting history facts at the same revision"));
            }
            return Ok(false);
        }
        if crate::job::is_terminal(&current.fact.status) {
            return Err(protocol("history cannot rewrite a terminal fact"));
        }
    }
    *existing = Some(incoming.clone());
    Ok(true)
}

fn output_matches(output: &CapturedOutput, fact: &Fact) -> bool {
    OutputRef::of(&output.stdout) == fact.stdout && OutputRef::of(&output.stderr) == fact.stderr
}

/// Validate then merge a record. Caller durably commits it with the page cursor.
/// This function never changes jobs, grants, requests or pairing.
pub fn ingest(store: &mut Store, published: &Published) -> Result<bool> {
    validate_record(store, &published.record)?;
    if let Some(output) = &published.output {
        let fact = published
            .record
            .selected()
            .ok_or_else(|| protocol("output has no history fact"))?;
        if !output_matches(output, &fact.fact) {
            return Err(protocol("history output differs from its signed digest"));
        }
    }
    let key = published.record.key()?;
    let mut entry = store.history.entries.get(&key).cloned().unwrap_or(Entry {
        record: Record {
            certificate: published.record.certificate.clone(),
            origin: None,
            runner: None,
        },
        change: 0,
        output: None,
    });
    if entry.record.certificate != published.record.certificate {
        return Err(protocol("different certificate for the same invocation"));
    }
    let mut changed = merge_fact(&mut entry.record.origin, &published.record.origin)?;
    changed |= merge_fact(&mut entry.record.runner, &published.record.runner)?;
    let local_output = entry.record.selected().is_some_and(|fact| {
        store.jobs.iter().any(|j| {
            store.job_certificates.get(&j.id) == Some(&key)
                && matches_job(&entry.record.certificate, j)
                && !store.output_pruned.contains(&j.id)
                && OutputRef::of(&j.stdout) == fact.fact.stdout
                && OutputRef::of(&j.stderr) == fact.fact.stderr
        })
    });
    if local_output {
        entry.output = None;
    } else if let Some(selected) = entry.record.selected() {
        if let Some(output) = published
            .output
            .as_ref()
            .filter(|_| !store.history_pruned.contains(&key))
        {
            if output_matches(output, &selected.fact) && entry.output.as_ref() != Some(output) {
                entry.output = Some(output.clone());
                changed = true;
            }
        }
        if entry
            .output
            .as_ref()
            .is_some_and(|o| !output_matches(o, &selected.fact))
        {
            entry.output = None;
            changed = true;
        }
    }
    if !store.history.entries.contains_key(&key) {
        changed = true;
    }
    if changed {
        entry.change = next_change(&mut store.history)?;
        store.history.entries.insert(key, entry);
    }
    Ok(changed)
}

pub(crate) fn normalize_output(store: &mut Store) {
    for (key, entry) in &mut store.history.entries {
        if store.history_pruned.contains(key) {
            entry.output = None;
        }
    }
    for job in &mut store.jobs {
        let Some(key) = store.job_certificates.get(&job.id) else {
            continue;
        };
        let Some(entry) = store.history.entries.get_mut(key) else {
            continue;
        };
        if !matches_job(&entry.record.certificate, job) {
            continue;
        }
        if store.history_pruned.contains(key) {
            job.stdout.clear();
            job.stderr.clear();
            if entry
                .record
                .selected()
                .is_some_and(|f| f.fact.stdout.bytes != 0 || f.fact.stderr.bytes != 0)
            {
                store.output_pruned.insert(job.id.clone());
            }
        } else if !store.output_pruned.contains(&job.id)
            && entry
                .output
                .as_ref()
                .is_some_and(|o| o.stdout == job.stdout && o.stderr == job.stderr)
        {
            entry.output = None;
        }
    }
}

pub fn for_job<'a>(store: &'a Store, job: &Job) -> Option<&'a Entry> {
    let key = store.job_certificates.get(&job.id)?;
    store.history.entries.get(key).filter(|e| {
        e.record.certificate.invocation.legacy_observer.is_none()
            && matches_job(&e.record.certificate, job)
    })
}

/// Select bytes without cloning an entire published record. Every caller uses
/// the same admitted identity, pruning and digest checks.
fn selected_output<'a>(
    store: &Store,
    entry: &'a Entry,
    local: Option<&'a Job>,
) -> Option<(&'a [u8], &'a [u8])> {
    let selected = entry.record.selected()?;
    let key = entry.record.key().ok()?;
    if store.history_pruned.contains(&key) {
        return None;
    }
    if let Some(job) = local.filter(|job| {
        store.job_certificates.get(&job.id) == Some(&key)
            && matches_job(&entry.record.certificate, job)
            && !store.output_pruned.contains(&job.id)
    }) {
        if OutputRef::of(&job.stdout) == selected.fact.stdout
            && OutputRef::of(&job.stderr) == selected.fact.stderr
        {
            return Some((&job.stdout, &job.stderr));
        }
    }
    entry
        .output
        .as_ref()
        .filter(|output| output_matches(output, &selected.fact))
        .map(|output| (output.stdout.as_slice(), output.stderr.as_slice()))
}

pub fn export_entry(store: &Store, entry: &Entry) -> Published {
    let id = &entry.record.certificate.invocation.id;
    let local = store.jobs.iter().find(|job| &job.id == id);
    let output = selected_output(store, entry, local).map(|(stdout, stderr)| CapturedOutput {
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
    });
    Published {
        record: entry.record.clone(),
        output,
        change: entry.change,
    }
}

pub fn page(store: &Store, epoch: &str, after: u64, high: Option<u64>) -> Result<Page> {
    let reset = epoch != store.history.epoch || after > store.history.clock;
    let after = if reset { 0 } else { after };
    let high = if reset {
        store.history.clock
    } else {
        high.unwrap_or(store.history.clock).min(store.history.clock)
    };
    if high < after {
        return Err(protocol("history cursor is beyond the scan boundary"));
    }
    let mut entries: Vec<_> = store
        .history
        .entries
        .values()
        .filter(|e| e.change > after && e.change <= high)
        .collect();
    entries.sort_by_key(|e| e.change);
    let mut published = Vec::new();
    let mut size = 0;
    let mut next = after;
    let total = entries.len();
    for entry in entries {
        let p = export_entry(store, entry);
        let bytes = serde_json::to_vec(&p)?.len();
        if bytes > PAGE_BYTES {
            return Err(protocol("one history record exceeds the page budget"));
        }
        if published.len() == PAGE_ROWS || size + bytes > PAGE_BYTES {
            break;
        }
        size += bytes;
        next = entry.change;
        published.push(p);
    }
    let complete = published.len() == total;
    if complete {
        next = high;
    }
    Ok(Page {
        epoch: store.history.epoch.clone(),
        reset,
        high,
        next,
        complete,
        entries: published,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub invocation: String,
    pub role: Role,
    pub legacy_observer: Option<BodyId>,
    pub status: JobStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub job: Job,
    pub invocation: Option<String>,
    pub retry_of: Option<String>,
    pub provenance: String,
    pub output_available: bool,
    pub stdout: OutputRef,
    pub stderr: OutputRef,
    pub observations: Vec<Observation>,
    /// This process's unsaved diagnostic, distinct from signed durable facts.
    pub local_warning: Option<String>,
}

fn view_group(job: &Job, retry_of: Option<&String>) -> String {
    // This is presentation grouping, not an admission or signature lookup.
    serde_json::to_string(&(&job.id, &job.from, &job.body, &job.argv, retry_of))
        .expect("string tuple serializes")
}

pub fn views(store: &Store) -> Vec<View> {
    projected_views(store, None)
}

pub(crate) fn views_for_job(store: &Store, id: &str) -> Vec<View> {
    projected_views(store, Some(id))
}

fn projected_views(store: &Store, id: Option<&str>) -> Vec<View> {
    let selected_job = |job: &&Job| id.is_none_or(|id| job.id == id);
    let local_by_id: BTreeMap<_, _> = store
        .jobs
        .iter()
        .enumerate()
        .filter(|(_, job)| selected_job(job))
        .map(|(index, job)| (job.id.as_str(), (index, job)))
        .collect();
    let mut rows: BTreeMap<String, (u8, View)> = BTreeMap::new();
    for job in store.jobs.iter().filter(selected_job) {
        let retry_of = store.retry_links.get(&job.id).cloned();
        let group = view_group(job, retry_of.as_ref());
        rows.insert(
            group,
            (
                0,
                View {
                    job: job.clone(),
                    invocation: None,
                    retry_of,
                    provenance: "local record; publication identity is unresolved".into(),
                    output_available: !store.output_pruned.contains(&job.id),
                    stdout: OutputRef::of(&job.stdout),
                    stderr: OutputRef::of(&job.stderr),
                    observations: Vec::new(),
                    local_warning: None,
                },
            ),
        );
    }
    for entry in store
        .history
        .entries
        .values()
        .filter(|entry| id.is_none_or(|id| entry.record.certificate.invocation.id == id))
    {
        let i = &entry.record.certificate.invocation;
        let Some(fact) = entry.record.selected() else {
            continue;
        };
        let output = selected_output(
            store,
            entry,
            local_by_id.get(i.id.as_str()).map(|(_, job)| *job),
        );
        let output_available =
            output.is_some() || (fact.fact.stdout.bytes == 0 && fact.fact.stderr.bytes == 0);
        let job = Job {
            id: i.id.clone(),
            from: i.from.clone(),
            body: i.body.clone(),
            argv: i.argv.clone(),
            status: fact.fact.status.clone(),
            stdout: output
                .map(|(stdout, _)| stdout.to_vec())
                .unwrap_or_default(),
            stderr: output
                .map(|(_, stderr)| stderr.to_vec())
                .unwrap_or_default(),
        };
        let key = entry.record.key().expect("serializable invocation");
        let (priority, provenance) = match &i.legacy_observer {
            Some(observer) => (
                if observer == &i.body { 3 } else { 1 },
                format!("legacy record observed by {observer}"),
            ),
            None if entry.record.runner.is_some() => (4, "verified runner result".into()),
            None => (
                2,
                "origin delivery observation; runner outcome not known".into(),
            ),
        };
        let observations: Vec<_> = [&entry.record.origin, &entry.record.runner]
            .into_iter()
            .flatten()
            .map(|f| Observation {
                invocation: key.clone(),
                role: f.fact.role,
                legacy_observer: i.legacy_observer.clone(),
                status: f.fact.status.clone(),
            })
            .collect();
        let group = view_group(&job, i.retry_of.as_ref());
        let mut row = View {
            job,
            invocation: Some(key),
            retry_of: i.retry_of.clone(),
            provenance,
            output_available,
            stdout: fact.fact.stdout.clone(),
            stderr: fact.fact.stderr.clone(),
            observations,
            local_warning: None,
        };
        if let Some((old_priority, old)) = rows.get_mut(&group) {
            if priority > *old_priority {
                row.observations.append(&mut old.observations);
                *old = row;
                *old_priority = priority;
            } else {
                old.observations.extend(row.observations);
            }
        } else {
            rows.insert(group, (priority, row));
        }
    }
    // A failed completion save must not make a dead local process appear to
    // remain Running merely because that is the latest durable signed fact.
    for job in store.jobs.iter().filter(selected_job).filter(|job| {
        job.body.0 == store.body_name && matches!(job.status, JobStatus::Uncertain { .. })
    }) {
        let group = view_group(job, store.retry_links.get(&job.id));
        if let Some((_, view)) = rows.get_mut(&group) {
            if view.job.status != job.status {
                view.job.status = job.status.clone();
                view.local_warning=Some("Local outcome is uncertain and has not been saved; the signed observations below remain the last durable evidence. Repair storage and restart the sidecar.".into());
            }
        }
    }
    let mut observed: Vec<_> = rows
        .into_values()
        .map(|(_, mut view)| {
            view.observations
                .sort_by(|a, b| a.invocation.cmp(&b.invocation));
            // This machine's selected entry sequence is display order only.
            // An unpublished local record falls back to its admission index.
            let sequence = view
                .invocation
                .as_ref()
                .and_then(|key| store.history.entries.get(key))
                .map(|entry| entry.change)
                .unwrap_or_else(|| {
                    local_by_id
                        .get(view.job.id.as_str())
                        .map_or(0, |(index, _)| *index as u64)
                });
            (sequence, view)
        })
        .collect();
    observed.sort_by(|(a, av), (b, bv)| a.cmp(b).then_with(|| av.job.id.cmp(&bv.job.id)));
    observed.into_iter().map(|(_, view)| view).collect()
}

pub fn projected(store: &Store) -> Vec<Job> {
    views(store).into_iter().map(|v| v.job).collect()
}

pub fn diagnostics(store: &Store) -> Value {
    let peers: Vec<_> = store.peers.iter().map(|peer| {
        let p = store.history.peers.get(&peer.name.0);
        json!({"body":peer.name,"complete":p.is_some_and(|p| p.complete && !p.unknown_authors && p.error.is_none() && p.last_contact.is_some()),
            "last_contact":p.and_then(|p| p.last_contact),"error":p.and_then(|p| p.error.as_ref()),
            "unknown_authors":p.is_some_and(|p| p.unknown_authors)})
    }).collect();
    json!({"peers":peers,"records":store.history.entries.len(),"local_sequence":store.history.clock,
        "unpublished_local_records":store.jobs.iter().filter(|j| !store.job_certificates.get(&j.id).is_some_and(|key|store.history.entries.contains_key(key))).count(),
        "legacy_records":store.history.entries.values().filter(|e|e.record.certificate.invocation.legacy_observer.is_some()).count()})
}

fn authors_known(store: &Store, c: &Certificate) -> bool {
    match &c.invocation.legacy_observer {
        Some(observer) => known_key(store, observer).is_ok(),
        None => {
            known_key(store, &c.invocation.from).is_ok()
                && known_key(store, &c.invocation.body).is_ok()
        }
    }
}

pub async fn sync_peer(store: &Arc<Mutex<Store>>, peer: &Peer) -> Result<()> {
    for _ in 0..16 {
        let (sk, progress, trust) = {
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            s.ensure_writable()?;
            (
                s.owner_sk.clone(),
                s.history
                    .peers
                    .get(&peer.name.0)
                    .cloned()
                    .unwrap_or_default(),
                s.history.trust.clone(),
            )
        };
        let high = (!progress.complete && !progress.epoch.is_empty()).then_some(progress.high);
        let response = crate::mesh::call(
            peer.addr.as_deref().ok_or(ClixError::Unreachable)?,
            &sk,
            &peer.owner_pk,
            json!({"op":"history_page","epoch":progress.epoch,"after":progress.cursor,"high":high}),
        )
        .await?;
        let page: Page = serde_json::from_value(response)?;
        if page.entries.len() > PAGE_ROWS
            || page.next > page.high
            || page.epoch.is_empty()
            || (!page.reset && page.epoch != progress.epoch)
            || (!page.reset && page.next < progress.cursor)
            || (page.complete && page.next != page.high)
        {
            return Err(protocol("invalid history page boundary"));
        }
        let mut previous = if page.reset { 0 } else { progress.cursor };
        for entry in &page.entries {
            if entry.change <= previous || entry.change > page.next {
                return Err(protocol("history page is not ordered"));
            }
            previous = entry.change;
        }
        let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
        let current = s
            .history
            .peers
            .get(&peer.name.0)
            .cloned()
            .unwrap_or_default();
        if s.history.trust != trust
            || current.epoch != progress.epoch
            || current.cursor != progress.cursor
            || current.high != progress.high
        {
            continue;
        }
        let changed = page.reset
            || page.next != progress.cursor
            || !page.entries.is_empty()
            || page.complete != progress.complete
            || page.high != progress.high;
        if changed {
            s.update(|s| {
                let mut unknown = if page.reset {
                    false
                } else {
                    progress.unknown_authors
                };
                for entry in &page.entries {
                    if authors_known(s, &entry.record.certificate) {
                        ingest(s, entry)?;
                    } else {
                        unknown = true;
                    }
                }
                s.history.peers.insert(
                    peer.name.0.clone(),
                    Progress {
                        epoch: page.epoch.clone(),
                        cursor: page.next,
                        high: page.high,
                        complete: page.complete,
                        unknown_authors: unknown,
                        last_contact: None,
                        error: None,
                    },
                );
                Ok(())
            })?;
        }
        let p = s.history.peers.entry(peer.name.0.clone()).or_default();
        p.last_contact = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        p.error = None;
        if page.complete {
            return Ok(());
        }
    }
    Ok(())
}
