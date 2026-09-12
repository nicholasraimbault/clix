use std::fs;
use std::io::Read;
use std::sync::{Arc, Mutex};

use crate::error::{ClixError, Result};
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
    store.append_job(job.clone())?;
    Ok(job)
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
