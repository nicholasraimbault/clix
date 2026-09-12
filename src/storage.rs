//! Bounded retained payloads, with permanent admission and delivery receipts.
//! Removing output cannot make an old invocation executable again.
use std::io::Write;

use serde_json::{json, Value};

use crate::error::{ClixError, Result};
use crate::store::Store;

pub(crate) const STATE_BYTES: usize = 128 * 1024 * 1024;
const OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const COMPLETION_RESERVE: usize = 2 * 1024 * 1024;

fn capacity(message: &str) -> ClixError {
    ClixError::Capacity(message.into())
}

struct BoundedJson {
    bytes: Vec<u8>,
    full: bool,
}

impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > STATE_BYTES {
            self.full = true;
            return Err(std::io::Error::other("state size limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn encode(store: &Store) -> Result<Vec<u8>> {
    let mut writer = BoundedJson {
        bytes: Vec::new(),
        full: false,
    };
    let result = serde_json::to_writer(&mut writer, store);
    if writer.full {
        return Err(capacity("Clix state exceeds 128 MiB; new work is refused"));
    }
    result?;
    Ok(writer.bytes)
}

pub(crate) fn output_bytes(store: &Store) -> usize {
    store
        .jobs
        .iter()
        .map(|j| j.stdout.len() + j.stderr.len())
        .sum::<usize>()
        + store
            .history
            .entries
            .values()
            .filter_map(|e| e.output.as_ref())
            .map(|o| o.stdout.len() + o.stderr.len())
            .sum::<usize>()
}

enum Payload {
    Local(String),
    Replica(String),
}

fn candidates(store: &Store) -> Vec<(u64, Payload, usize)> {
    // Count one invocation once, even when an older state retained both its
    // admitted payload and a replica cache. Removing Local clears both copies.
    let mut candidates = std::collections::BTreeMap::new();
    for job in &store.jobs {
        let bytes = job.stdout.len() + job.stderr.len();
        if bytes == 0 || !crate::job::is_terminal(&job.status) {
            continue;
        }
        if let Some(key) = store.job_certificates.get(&job.id) {
            if let Some(entry) = store.history.entries.get(key) {
                if entry.record.selected().is_some() {
                    candidates.insert(
                        key.clone(),
                        (entry.change, Payload::Local(job.id.clone()), bytes),
                    );
                }
            }
        }
    }
    for (key, entry) in &store.history.entries {
        let bytes = entry
            .output
            .as_ref()
            .map_or(0, |o| o.stdout.len() + o.stderr.len());
        if bytes > 0
            && entry
                .record
                .selected()
                .is_some_and(|f| crate::job::is_terminal(&f.fact.status))
        {
            candidates
                .entry(key.clone())
                .and_modify(|(_, _, n)| *n += bytes)
                .or_insert_with(|| (entry.change, Payload::Replica(key.clone()), bytes));
        }
    }
    let mut candidates: Vec<_> = candidates.into_values().collect();
    candidates.sort_by_key(|(age, _, _)| *age);
    candidates
}

fn remove_payload(store: &mut Store, payload: Payload) -> Result<()> {
    let key = match payload {
        Payload::Local(id) => {
            let key = store
                .job_certificates
                .get(&id)
                .cloned()
                .ok_or_else(|| ClixError::Protocol("output has no admission record".into()))?;
            let job = store
                .jobs
                .iter_mut()
                .find(|j| j.id == id)
                .ok_or_else(|| ClixError::Protocol("output job disappeared".into()))?;
            job.stdout.clear();
            job.stderr.clear();
            store.output_pruned.insert(id);
            key
        }
        Payload::Replica(key) => key,
    };
    let change = store
        .history
        .clock
        .checked_add(1)
        .ok_or_else(|| ClixError::Protocol("history sequence exhausted".into()))?;
    store.history.clock = change;
    if let Some(entry) = store.history.entries.get_mut(&key) {
        entry.output = None;
        entry.change = change;
    }
    store.history_pruned.insert(key);
    Ok(())
}

/// Explicit owner pruning. Facts, invocation identities, terminal outcomes and
/// both kinds of replay receipt survive; active jobs are never selected.
pub(crate) fn prune(store: &mut Store, keep: usize) -> Result<usize> {
    let before = output_bytes(store);
    let candidates = candidates(store);
    let count = candidates.len().saturating_sub(keep);
    for (_, payload, _) in candidates.into_iter().take(count) {
        remove_payload(store, payload)?;
    }
    Ok(before - output_bytes(store))
}

fn count_limit(name: &str, next: usize, old: usize, max: usize) -> Result<()> {
    if next > max && next > old {
        return Err(capacity(&format!("{name} limit ({max}) reached; new admissions are refused, existing records are preserved")));
    }
    Ok(())
}

pub(crate) fn prepare(next: &mut Store, old: &Store) -> Result<()> {
    count_limit("execution receipt", next.jobs.len(), old.jobs.len(), 10_000)?;
    count_limit(
        "history record",
        next.history.entries.len(),
        old.history.entries.len(),
        30_000,
    )?;
    count_limit(
        "request delivery receipt",
        next.request_receipts.len(),
        old.request_receipts.len(),
        10_000,
    )?;
    count_limit(
        "pending request",
        next.requests.len(),
        old.requests.len(),
        1024,
    )?;
    count_limit(
        "outbound request",
        next.outbound_requests.len(),
        old.outbound_requests.len(),
        1024,
    )?;
    count_limit("paired machine", next.peers.len(), old.peers.len(), 64)?;
    count_limit("grant", next.grants.len(), old.grants.len(), 256)?;
    if output_bytes(next) > OUTPUT_BYTES {
        for (_, payload, _) in candidates(next) {
            if output_bytes(next) <= OUTPUT_BYTES {
                break;
            }
            remove_payload(next, payload)?;
        }
        if output_bytes(next) > OUTPUT_BYTES {
            return Err(capacity(
                "retained output budget is full; active or unverified data was preserved",
            ));
        }
    }
    Ok(())
}

fn payload_json_bytes(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    bytes
        .iter()
        .map(|b| {
            if *b >= 100 {
                3
            } else if *b >= 10 {
                2
            } else {
                1
            }
        })
        .sum::<usize>()
        + bytes.len()
        - 1
}

pub(crate) fn completion_charge(store: &Store, encoded_bytes: usize) -> usize {
    let payload = store
        .jobs
        .iter()
        .map(|j| payload_json_bytes(&j.stdout) + payload_json_bytes(&j.stderr))
        .sum::<usize>()
        + store
            .history
            .entries
            .values()
            .filter_map(|e| e.output.as_ref())
            .map(|o| payload_json_bytes(&o.stdout) + payload_json_bytes(&o.stderr))
            .sum::<usize>();
    let metadata = encoded_bytes.saturating_sub(payload);
    // Pruning creates permanent receipts. Reserve each absent JSON string plus
    // its possible comma now; inserting it cannot increase the later charge.
    let prune_receipts = store
        .jobs
        .iter()
        .filter(|j| !store.output_pruned.contains(&j.id))
        .map(|j| {
            serde_json::to_string(&j.id)
                .expect("string serializes")
                .len()
                + 1
        })
        .sum::<usize>()
        + store
            .history
            .entries
            .keys()
            .filter(|key| !store.history_pruned.contains(*key))
            .map(|key| serde_json::to_string(key).expect("string serializes").len() + 1)
            .sum::<usize>();
    // Pruning increments the global and entry sequences. Reserve their full
    // u64 decimal width now, including carries such as 9 -> 10 and 99 -> 100.
    let sequence_headroom =
        |n: u64| 20 - n.checked_ilog10().map_or(1, |digits| digits as usize + 1);
    let sequences = sequence_headroom(store.history.clock)
        + store
            .history
            .entries
            .values()
            .map(|entry| sequence_headroom(entry.change))
            .sum::<usize>();
    let pending = store
        .jobs
        .iter()
        .filter(|j| !crate::job::is_terminal(&j.status))
        .count();
    // Reserve the full worst-case output budget independently of the retained
    // bytes' digit widths. Pruning cannot increase this capacity charge.
    metadata
        .saturating_add(prune_receipts)
        .saturating_add(sequences)
        .saturating_add(OUTPUT_BYTES * 4)
        .saturating_add(pending.saturating_mul(COMPLETION_RESERVE))
}

pub(crate) fn reserve_completion(store: &Store, old: &Store, encoded_bytes: usize) -> Result<()> {
    let charge = completion_charge(store, encoded_bytes);
    if charge <= STATE_BYTES {
        return Ok(());
    }
    // An upgraded state may already exceed this policy. Preserve inspection,
    // removals and completion while refusing updates that increase its charge.
    // This additional encoding occurs only for the exceptional over-quota case.
    let old_charge = completion_charge(old, encode(old)?.len());
    if charge <= old_charge {
        return Ok(());
    }
    Err(capacity(
        "state capacity is reserved for admitted job outcomes; new work is refused",
    ))
}

pub(crate) fn status(store: &Store) -> Value {
    json!({"state_limit_bytes":STATE_BYTES,"retained_output_limit_bytes":OUTPUT_BYTES,
        "retained_output_bytes":output_bytes(store),"admitted_job_records":store.jobs.len(),
        "execution_receipt_limit":10000,"request_receipts":store.request_receipts.len(),
        "request_receipt_limit":10000,"history_records":store.history.entries.len(),
        "history_record_limit":30000,"storage_error":store.storage_error(),
        "running_children_limit":crate::limits::CHILDREN,"outbound_attempt_limit":crate::limits::DISPATCH,
        "pruning":"Only saved output is pruned; invocation and delivery receipts remain to reject replays."})
}
