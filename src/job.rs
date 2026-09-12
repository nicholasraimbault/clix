use std::fs;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::error::{ClixError, Result};
use crate::mesh;
use crate::store::Store;
use crate::types::{BodyId, Job, JobStatus};

/// UUID v4 from `/dev/urandom`.
pub fn new_id() -> Result<String> {
    let mut buf = [0u8; 16];
    let mut f = fs::File::open("/dev/urandom")
        .map_err(|e| ClixError::Io(format!("read /dev/urandom: {e}")))?;
    f.read_exact(&mut buf)
        .map_err(|e| ClixError::Io(format!("read /dev/urandom: {e}")))?;
    buf[6] = (buf[6] & 0x0f) | 0x40;
    buf[8] = (buf[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10],
        buf[11], buf[12], buf[13], buf[14], buf[15]
    ))
}

pub fn append(
    store: &Arc<Mutex<Store>>,
    from: BodyId,
    body: BodyId,
    argv: Vec<String>,
    status: JobStatus,
) -> Result<Job> {
    append_retry(store, from, body, argv, status, None)
}

pub(crate) fn append_retry(
    store: &Arc<Mutex<Store>>,
    from: BodyId,
    body: BodyId,
    argv: Vec<String>,
    status: JobStatus,
    retry_of: Option<String>,
) -> Result<Job> {
    let id = new_id()?;
    crate::limits::invocation(&id, &argv)?;
    let job = Job {
        id,
        body,
        argv,
        from,
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    crate::limits::invocation(&job.id, &job.argv)?;
    store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .update(|s| {
            if let Some(old) = retry_of {
                s.retry_links.insert(job.id.clone(), old);
            }
            s.jobs.push(job.clone());
            Ok(())
        })?;
    Ok(job)
}

pub fn asleep_message(body: &str) -> String {
    format!("{body} is asleep, waiting…")
}

pub fn waiting_json(body: &str, job: &Job) -> Value {
    json!({"ok": true, "status": "waiting", "waiting": asleep_message(body), "job": job})
}

pub fn exec_json(job: &Job) -> Value {
    match &job.status {
        JobStatus::Done { exit } => json!({"status":"done", "exit":exit, "job":job}),
        JobStatus::Denied { reason } => json!({"status":"denied", "reason":reason, "job":job}),
        JobStatus::Failed { reason } => json!({"status":"failed", "reason":reason, "job":job}),
        JobStatus::Uncertain { reason } => {
            json!({"status":"uncertain", "reason":reason, "job":job})
        }
        JobStatus::WaitingBody => waiting_json(&job.body.0, job),
        JobStatus::WaitingCapacity => {
            json!({"ok":true,"status":"capacity_wait","waiting":format!("{} is busy; this job is saved and waiting for capacity",job.body),"job":job})
        }
        JobStatus::Running => json!({"status":"running", "job":job}),
        JobStatus::Queued => json!({"status":"queued", "job":job}),
    }
}

pub fn is_terminal(status: &JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Done { .. }
            | JobStatus::Denied { .. }
            | JobStatus::Failed { .. }
            | JobStatus::Uncertain { .. }
    )
}

pub fn poll_interval() -> Duration {
    let raw = std::env::var("CLIX_WAIT_POLL").unwrap_or_default();
    let seconds = raw
        .strip_suffix("ms")
        .and_then(|s| s.parse::<f64>().ok())
        .map(|n| n / 1000.0)
        .or_else(|| raw.trim_end_matches('s').parse::<f64>().ok())
        .unwrap_or(2.0);
    Duration::from_secs_f64(if seconds.is_finite() {
        seconds.clamp(0.01, 60.0)
    } else {
        2.0
    })
}

pub fn is_unreachable(err: &ClixError) -> bool {
    matches!(err, ClixError::Unreachable)
}

pub fn apply_peer_result(
    store: &Arc<Mutex<Store>>,
    peer: &BodyId,
    incoming: &Job,
) -> Result<Option<Job>> {
    crate::limits::result(incoming)?;
    let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
    let Some(existing) = s.jobs.iter().find(|j| j.id == incoming.id) else {
        return Ok(None);
    };
    if existing.body != *peer || is_terminal(&existing.status) {
        return Ok(None);
    }
    if incoming.body != *peer || incoming.from != existing.from || incoming.argv != existing.argv {
        return Err(ClixError::Protocol(
            "peer returned a different job invocation".into(),
        ));
    }
    if incoming.status == existing.status
        && incoming.stdout == existing.stdout
        && incoming.stderr == existing.stderr
    {
        return Ok(Some(existing.clone()));
    }
    s.put_job(incoming.clone())?;
    Ok(Some(incoming.clone()))
}

/// Fair, bounded attempts. Queued work stays durable in Store; no task waits
/// for a dispatcher permit. Only the selected rows are cloned.
pub(crate) async fn dispatch_pending(
    store: Arc<Mutex<Store>>,
    woke: Arc<Notify>,
    limits: Arc<crate::limits::Limits>,
) {
    use std::collections::{BTreeMap, HashMap};
    use tokio::time::Instant;

    enum Work<'a> {
        Job(&'a Job),
        Request(&'a crate::types::OutboundRequest),
        History(&'a crate::types::Peer),
        Local(&'a Job),
    }

    let mut tasks = tokio::task::JoinSet::new();
    let mut active: HashMap<tokio::task::Id, String> = HashMap::new();
    let mut next_attempt: BTreeMap<String, Instant> = BTreeMap::new();
    let mut cursor = String::new();
    loop {
        {
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            let mut candidates = Vec::new();
            for j in &s.jobs {
                if j.from.0 == s.body_name && j.body.0 != s.body_name && !is_terminal(&j.status) {
                    candidates.push((format!("job:{}", j.id), Work::Job(j)));
                } else if j.from.0 == s.body_name
                    && j.body.0 == s.body_name
                    && matches!(j.status, JobStatus::Queued | JobStatus::WaitingCapacity)
                {
                    candidates.push((format!("local:{}", j.id), Work::Local(j)));
                }
            }
            for request in s.outbound_requests.iter().filter(|r| r.error.is_none()) {
                candidates.push((format!("request:{}", request.id), Work::Request(request)));
            }
            for peer in &s.peers {
                candidates.push((format!("history:{}", peer.name), Work::History(peer)));
            }
            candidates.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            next_attempt.retain(|key, _| candidates.binary_search_by(|(k, _)| k.cmp(key)).is_ok());
            let start = candidates.partition_point(|(key, _)| key <= &cursor);
            let now = Instant::now();
            let mut last = None;
            for (key, work) in candidates[start..].iter().chain(candidates[..start].iter()) {
                if tasks.len() >= crate::limits::DISPATCH {
                    break;
                }
                if active.values().any(|running| running == key)
                    || next_attempt.get(key).is_some_and(|when| *when > now)
                {
                    continue;
                }
                let store = store.clone();
                let handle = match work {
                    Work::Job(j) => {
                        let j = (*j).clone();
                        tasks.spawn(async move {
                            if let Err(e) = dispatch_one(&store, &j).await {
                                eprintln!("could not update job {}: {e}", j.id);
                            }
                        })
                    }
                    Work::Request(request) => {
                        let request = (*request).clone();
                        tasks.spawn(async move {
                            if let Err(e) = crate::request::deliver(&store, &request).await {
                                eprintln!("could not update request {}: {e}", request.id);
                            }
                        })
                    }
                    Work::History(peer) => {
                        let peer = (*peer).clone();
                        tasks.spawn(async move {
                            if let Err(e) = crate::history::sync_peer(&store, &peer).await {
                                let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
                                let p = s.history.peers.entry(peer.name.0).or_default();
                                p.complete = false;
                                p.error = Some(if matches!(e, ClixError::Unreachable) {
                                    "unreachable; saved history may be stale".into()
                                } else {
                                    e.to_string()
                                });
                            }
                        })
                    }
                    Work::Local(job) => {
                        let job = (*job).clone();
                        let limits = limits.clone();
                        tasks.spawn(async move {
                            if let Err(error) = crate::exec::submit(
                                &store,
                                &limits,
                                &job.from,
                                &job.argv,
                                job.id.clone(),
                            ) {
                                let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
                                let result = s.update(|s| {
                                    let j = s.jobs.iter_mut().find(|j| j.id == job.id).ok_or_else(
                                        || {
                                            ClixError::Protocol(
                                                "queued local job disappeared".into(),
                                            )
                                        },
                                    )?;
                                    if !matches!(
                                        j.status,
                                        JobStatus::Queued | JobStatus::WaitingCapacity
                                    ) {
                                        return Ok(());
                                    }
                                    j.status = if matches!(error, ClixError::Capacity(_)) {
                                        JobStatus::WaitingCapacity
                                    } else {
                                        JobStatus::Failed {
                                            reason: error.to_string(),
                                        }
                                    };
                                    Ok(())
                                });
                                if let Err(e) = result {
                                    eprintln!("could not update queued local job: {e}");
                                }
                            }
                        })
                    }
                };
                active.insert(handle.id(), key.clone());
                next_attempt.insert(key.clone(), now + poll_interval());
                last = Some(key.clone());
            }
            if let Some(last) = last {
                cursor = last;
            }
        }
        tokio::select! {
            result = tasks.join_next_with_id(), if !tasks.is_empty() => {
                let finished = match result {
                    Some(Ok((id, ()))) => Some(id),
                    Some(Err(e)) => {
                        eprintln!("job dispatcher task failed: {e}");
                        Some(e.id())
                    }
                    None => None,
                };
                if let Some(id) = finished {
                    active.remove(&id);
                }
            }
            _ = tokio::time::sleep(poll_interval()) => {}
            _ = woke.notified() => {}
        }
    }
}

async fn dispatch_one(store: &Arc<Mutex<Store>>, job: &Job) -> Result<()> {
    let (peer, sk, certificate) = {
        let s = store.lock().unwrap_or_else(|e| e.into_inner());
        s.ensure_writable()?;
        (
            s.peers.iter().find(|p| p.name == job.body).cloned(),
            s.owner_sk.clone(),
            crate::history::for_job(&s, job).map(|e| e.record.certificate.clone()),
        )
    };
    let outcome = async {
        let peer = peer.ok_or_else(|| ClixError::Usage(format!("{} is not paired", job.body)))?;
        let addr = peer.addr.as_deref().ok_or(ClixError::Unreachable)?;
        let query = certificate.as_ref().map_or_else(
            || json!({"op":"job_get","job_id":job.id}),
            |c| json!({"op":"job_get_v2","invocation":c}),
        );
        let apply = |response: &Value| -> Result<()> {
            if certificate.is_some() {
                let published: crate::history::Published =
                    serde_json::from_value(response.get("history").cloned().ok_or_else(|| {
                        ClixError::Protocol("peer response is missing signed history".into())
                    })?)?;
                store
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .update(|s| crate::history::accept_runner_reply(s, &peer.name, &published))?;
            } else {
                let job: Job = serde_json::from_value(response["job"].clone())?;
                apply_peer_result(store, &peer.name, &job)?;
            }
            Ok(())
        };
        let known = mesh::call(addr, &sk, &peer.owner_pk, query).await?;
        if known.get("job").is_some_and(|v| !v.is_null()) {
            apply(&known)?;
            return Ok(());
        }
        {
            crate::pin::sync_with_peer(store, &sk, addr, &peer.name.0).await?;
        }
        let resp = mesh::call(
            addr,
            &sk,
            &peer.owner_pk,
            certificate.as_ref().map_or_else(
                || json!({"op":"exec","argv":job.argv,"job_id":job.id}),
                |c| json!({"op":"exec_v2","invocation":c}),
            ),
        )
        .await?;
        apply(&resp)?;
        Ok(())
    }
    .await;
    if let Err(e) = outcome {
        let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
        let Some(existing) = s.jobs.iter().find(|j| j.id == job.id) else {
            return Ok(());
        };
        if is_terminal(&existing.status) {
            return Ok(());
        }
        let mut updated = existing.clone();
        if matches!(e, ClixError::Capacity(_)) {
            // The runner rejected admission. Keep this exact ID retryable.
            // A known Running job must never regress because of a later error.
            if !matches!(updated.status, JobStatus::Running) {
                updated.status = JobStatus::WaitingCapacity;
            }
        } else if is_unreachable(&e) {
            // A known running job remains running: loss of connectivity is not
            // evidence that execution stopped. Redelivery still uses the same ID.
            if !matches!(updated.status, JobStatus::Running) {
                updated.status = JobStatus::WaitingBody;
            }
        } else {
            updated.status = JobStatus::Failed {
                reason: e.to_string(),
            };
        }
        if updated.status != existing.status {
            s.put_job(updated)?;
        }
    }
    Ok(())
}

pub fn format_line(job: &Job) -> String {
    let tail = match &job.status {
        JobStatus::Done { exit } => format!("exit {exit}"),
        JobStatus::Denied { reason } => format!("denied: {reason}"),
        JobStatus::Failed { reason } => format!("failed: {reason}"),
        JobStatus::Uncertain { reason } => format!("uncertain: {reason}"),
        JobStatus::WaitingBody => "waiting".into(),
        JobStatus::WaitingCapacity => "waiting for capacity".into(),
        JobStatus::Running => "running".into(),
        JobStatus::Queued => "queued".into(),
    };
    format!(
        "{:?}  {} {:?} on {}  {tail}",
        job.id, job.from, job.argv, job.body
    )
}
