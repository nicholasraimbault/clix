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
    };
    let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
    store.put_job(job.clone())?;
    Ok(job)
}

pub fn put(store: &Arc<Mutex<Store>>, job: Job) -> Result<Job> {
    let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
    store.put_job(job.clone())?;
    Ok(job)
}

pub fn asleep_message(body: &str) -> String {
    format!("{body} is asleep, waiting…")
}

pub fn waiting_json(body: &str, job: &Job) -> Value {
    json!({
        "ok": true,
        "status": "waiting",
        "waiting": asleep_message(body),
        "job": job,
    })
}

pub fn exec_json(job: &Job) -> Value {
    match &job.status {
        JobStatus::Done { exit } => json!({
            "status": "done",
            "exit": exit,
            "stdout": "",
            "stderr": "",
            "job": job,
        }),
        JobStatus::Denied { reason } => json!({
            "status": "denied",
            "reason": reason,
            "job": job,
        }),
        JobStatus::Failed { reason } => json!({
            "status": "failed",
            "reason": reason,
            "job": job,
        }),
        JobStatus::WaitingBody => waiting_json(&job.body.0, job),
        JobStatus::Running => json!({
            "status": "running",
            "job": job,
        }),
    }
}

pub fn is_terminal(status: &JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Done { .. } | JobStatus::Denied { .. } | JobStatus::Failed { .. }
    )
}

/// `CLIX_WAIT_POLL` default 2s. Accepts `2`, `2s`, `50ms`.
pub fn poll_interval() -> Duration {
    let raw = std::env::var("CLIX_WAIT_POLL").unwrap_or_default();
    parse_poll(&raw).unwrap_or(Duration::from_secs(2))
}

fn parse_poll(raw: &str) -> Option<Duration> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(ms) = s.strip_suffix("ms") {
        return ms
            .parse::<u64>()
            .ok()
            .map(|n| Duration::from_millis(n.max(1)));
    }
    if let Some(rest) = s.strip_suffix('s') {
        if let Ok(n) = rest.parse::<u64>() {
            return Some(Duration::from_secs(n.max(1)));
        }
        if let Ok(f) = rest.parse::<f64>() {
            return Some(Duration::from_secs_f64(f.max(0.001)));
        }
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(Duration::from_secs(n.max(1)));
    }
    s.parse::<f64>()
        .ok()
        .map(|f| Duration::from_secs_f64(f.max(0.001)))
}

pub fn is_unreachable(err: &ClixError) -> bool {
    let ClixError::Io(s) = err else {
        return false;
    };
    let s = s.to_ascii_lowercase();
    s.contains("connection refused")
        || s.contains("connection reset")
        || s.contains("connection aborted")
        || s.contains("broken pipe")
        || s.contains("timed out")
        || s.contains("timeout")
        || s.contains("network is unreachable")
        || s.contains("host is unreachable")
        || s.contains("no route to host")
        || s.contains("not connected")
        || s.contains("os error 111")
        || s.contains("os error 104")
        || s.contains("os error 110")
        || s.contains("empty mesh response")
}

/// Finish a waiting/running job destined to `peer`. Keeps stored from/body/argv.
pub fn apply_peer_result(
    store: &Arc<Mutex<Store>>,
    peer: &BodyId,
    incoming: &Job,
) -> Result<Option<Job>> {
    let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
    let Some(existing) = s.jobs.iter_mut().find(|j| j.id == incoming.id) else {
        return Ok(None);
    };
    if existing.body != *peer {
        return Ok(None);
    }
    if !matches!(existing.status, JobStatus::WaitingBody | JobStatus::Running) {
        return Ok(None);
    }
    if !is_terminal(&incoming.status) {
        return Ok(None);
    }
    existing.status = incoming.status.clone();
    let out = existing.clone();
    s.save()?;
    Ok(Some(out))
}

/// Retry mesh exec until the body is back or the job is already terminal.
/// Mesh-retry only while `WaitingBody`. `Running` waits for `job_result`.
/// Grant check happens on the runner at run time.
pub async fn wait_for_peer(
    store: &Arc<Mutex<Store>>,
    woke: &Notify,
    body: &str,
    sk: &[u8],
    argv: &[String],
    job: &Job,
) -> Result<Value> {
    let dest = BodyId(body.to_string());
    loop {
        let retry = {
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            match s.jobs.iter().find(|j| j.id == job.id) {
                Some(existing) if is_terminal(&existing.status) => {
                    return Ok(exec_json(existing));
                }
                Some(existing) if matches!(existing.status, JobStatus::WaitingBody) => true,
                _ => false,
            }
        };
        if retry {
            let addr = {
                let s = store.lock().unwrap_or_else(|e| e.into_inner());
                s.peers
                    .iter()
                    .find(|p| p.name.0 == body)
                    .and_then(|p| p.addr.clone())
            };
            if let Some(addr) = addr {
                match mesh::call(
                    &addr,
                    sk,
                    json!({"op": "exec", "argv": argv, "job_id": job.id}),
                )
                .await
                {
                    Ok(resp) => {
                        let st = resp.get("status").and_then(Value::as_str);
                        if st != Some("running") && st != Some("waiting") {
                            if let Some(v) = resp.get("job") {
                                if let Ok(remote) = serde_json::from_value::<Job>(v.clone()) {
                                    apply_peer_result(store, &dest, &remote)?;
                                }
                            }
                            if st == Some("done") || st == Some("denied") || st == Some("failed") {
                                return Ok(resp);
                            }
                        }
                    }
                    Err(e) if is_unreachable(&e) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(poll_interval()) => {}
            _ = woke.notified() => {}
        }
    }
}

pub fn format_line(job: &Job) -> String {
    let cmd = job.argv.join(" ");
    let tail = match &job.status {
        JobStatus::Done { exit } => format!("exit {exit}"),
        JobStatus::Denied { reason } => format!("denied: {reason}"),
        JobStatus::Failed { reason } => format!("failed: {reason}"),
        JobStatus::WaitingBody => "waiting".to_string(),
        JobStatus::Running => "running".to_string(),
    };
    format!("{} {cmd} on {}  {tail}", job.from, job.body)
}
