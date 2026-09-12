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
    let job = Job {
        id: new_id()?,
        body,
        argv,
        from,
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    put(store, job)
}

pub fn put(store: &Arc<Mutex<Store>>, job: Job) -> Result<Job> {
    store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .put_job(job.clone())?;
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

/// One dispatcher owns outbound delivery, including restoration after restart.
/// Every attempt uses the original durable job ID; the receiver deduplicates it.
pub async fn dispatch_pending(store: Arc<Mutex<Store>>, woke: Arc<Notify>) {
    let mut tasks = tokio::task::JoinSet::new();
    let mut active = std::collections::HashSet::new();
    loop {
        let pending: Vec<Job> = {
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            s.jobs
                .iter()
                .filter(|j| {
                    j.from.0 == s.body_name && j.body.0 != s.body_name && !is_terminal(&j.status)
                })
                .cloned()
                .collect()
        };
        for j in pending {
            if active.insert(j.id.clone()) {
                let s = store.clone();
                tasks.spawn(async move {
                    if let Err(e) = dispatch_one(&s, &j).await {
                        eprintln!("could not update job {}: {e}", j.id);
                    }
                    j.id
                });
            }
        }
        let requests = store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .outbound_requests
            .clone();
        for request in requests.into_iter().filter(|r| r.error.is_none()) {
            let id = format!("request:{}", request.id);
            if active.insert(id.clone()) {
                let s = store.clone();
                tasks.spawn(async move {
                    if let Err(e) = crate::request::deliver(&s, &request).await {
                        eprintln!("could not update request {}: {e}", request.id);
                    }
                    id
                });
            }
        }
        tokio::select! {
            result = tasks.join_next(), if !tasks.is_empty() => {
                match result {
                    Some(Ok(id)) => { active.remove(&id); }
                    Some(Err(e)) => { eprintln!("job dispatcher task failed: {e}"); active.clear(); }
                    None => {}
                }
                // Avoid immediately retrying a reachable running job in a busy loop.
                tokio::time::sleep(poll_interval()).await;
            }
            _ = tokio::time::sleep(poll_interval()) => {}
            _ = woke.notified() => {}
        }
    }
}

async fn dispatch_one(store: &Arc<Mutex<Store>>, job: &Job) -> Result<()> {
    let (peer, sk) = {
        let s = store.lock().unwrap_or_else(|e| e.into_inner());
        (
            s.peers.iter().find(|p| p.name == job.body).cloned(),
            s.owner_sk.clone(),
        )
    };
    let outcome = async {
        let peer = peer.ok_or_else(|| ClixError::Usage(format!("{} is not paired", job.body)))?;
        let addr = peer.addr.as_deref().ok_or(ClixError::Unreachable)?;
        let known = mesh::call(
            addr,
            &sk,
            &peer.owner_pk,
            json!({"op":"job_get","job_id":job.id}),
        )
        .await?;
        if let Some(value) = known.get("job").filter(|v| !v.is_null()) {
            let existing: Job = serde_json::from_value(value.clone())?;
            apply_peer_result(store, &peer.name, &existing)?;
            return Ok(());
        }
        {
            crate::pin::sync_with_peer(store, &sk, addr, &peer.name.0).await?;
        }
        let resp = mesh::call(
            addr,
            &sk,
            &peer.owner_pk,
            json!({"op":"exec","argv":job.argv,"job_id":job.id}),
        )
        .await?;
        let v = resp
            .get("job")
            .ok_or_else(|| ClixError::Protocol("peer response is missing its job".into()))?;
        let received: Job = serde_json::from_value(v.clone())?;
        apply_peer_result(store, &peer.name, &received)?;
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
        if is_unreachable(&e) {
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
        JobStatus::Running => "running".into(),
        JobStatus::Queued => "queued".into(),
    };
    format!(
        "{:?}  {} {:?} on {}  {tail}",
        job.id, job.from, job.argv, job.body
    )
}
