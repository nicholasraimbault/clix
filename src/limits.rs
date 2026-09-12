//! Initial admission policy. These limits bound Clix's own work; they are not
//! measured throughput promises or a sandbox for a granted tool's descendants.
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::{ClixError, Result};

pub(crate) const CHILDREN: usize = 4;
pub(crate) const DISPATCH: usize = 8;
// Exec may call back to the other body for pin RPCs. Bound waiting exec
// requests separately, leaving connection slots for those non-recursive RPCs.
pub(crate) const MESH_CONNECTIONS: usize = 16;
pub(crate) const MESH_EXEC_REQUESTS: usize = 4;
pub(crate) const OWNER_CONNECTIONS: usize = 32;
pub(crate) const OWNER_WAITERS: usize = 8;
pub(crate) const ARGV_COUNT: usize = 256;
pub(crate) const ARGV_BYTES: usize = 64 * 1024;
pub(crate) const ID_BYTES: usize = 128;
pub(crate) const TOOL_BYTES: usize = 4096;
pub(crate) const STATUS_REASON_BYTES: usize = 64 * 1024;
pub(crate) const OUTPUT_STREAM_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const MESH_FRAME_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const OWNER_REQUEST_BYTES: usize = 128 * 1024;
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(crate) struct Limits {
    children: Arc<Semaphore>,
    mesh: Arc<Semaphore>,
    mesh_exec: Arc<Semaphore>,
    owner: Arc<Semaphore>,
    waiters: Arc<Semaphore>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            children: Arc::new(Semaphore::new(CHILDREN)),
            mesh: Arc::new(Semaphore::new(MESH_CONNECTIONS)),
            mesh_exec: Arc::new(Semaphore::new(MESH_EXEC_REQUESTS)),
            owner: Arc::new(Semaphore::new(OWNER_CONNECTIONS)),
            waiters: Arc::new(Semaphore::new(OWNER_WAITERS)),
        }
    }
}

impl Limits {
    pub(crate) fn mesh_exec(&self) -> Result<OwnedSemaphorePermit> {
        self.mesh_exec.clone().try_acquire_owned().map_err(|_| {
            ClixError::Capacity("execution admission is busy; waiting for capacity".into())
        })
    }

    pub(crate) fn child(&self) -> Result<OwnedSemaphorePermit> {
        self.children.clone().try_acquire_owned().map_err(|_| {
            ClixError::Capacity("execution slots are busy; waiting for capacity".into())
        })
    }

    pub(crate) fn waiter(&self) -> Result<OwnedSemaphorePermit> {
        self.waiters.clone().try_acquire_owned().map_err(|_| {
            ClixError::Capacity("too many clients are waiting; inspect the saved job by ID".into())
        })
    }

    pub(crate) async fn mesh(&self) -> OwnedSemaphorePermit {
        self.mesh
            .clone()
            .acquire_owned()
            .await
            .expect("mesh limits are never closed")
    }

    pub(crate) async fn owner(&self) -> OwnedSemaphorePermit {
        self.owner
            .clone()
            .acquire_owned()
            .await
            .expect("owner limits are never closed")
    }
}

pub(crate) fn invocation(id: &str, argv: &[String]) -> Result<()> {
    identifier(id)?;
    if argv.is_empty() || argv.len() > ARGV_COUNT {
        return Err(ClixError::Usage(
            "command requires between 1 and 256 arguments".into(),
        ));
    }
    tool(&argv[0])?;
    let mut bytes = 0usize;
    for arg in argv {
        if arg.contains('\0') {
            return Err(ClixError::Usage(
                "command arguments must not contain NUL".into(),
            ));
        }
        bytes = bytes
            .checked_add(arg.len())
            .ok_or_else(|| ClixError::Usage("command arguments exceed 64 KiB".into()))?;
        if bytes > ARGV_BYTES {
            return Err(ClixError::Usage("command arguments exceed 64 KiB".into()));
        }
    }
    Ok(())
}

pub(crate) fn identifier(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > ID_BYTES || id.contains('\0') {
        return Err(ClixError::Usage(
            "identifier requires 1 to 128 bytes and no NUL".into(),
        ));
    }
    Ok(())
}

pub(crate) fn status(value: &crate::types::JobStatus) -> Result<()> {
    use crate::types::JobStatus;
    match value {
        JobStatus::Denied { reason }
        | JobStatus::Failed { reason }
        | JobStatus::Uncertain { reason }
            if reason.len() > STATUS_REASON_BYTES =>
        {
            Err(ClixError::Protocol("job reason exceeds 64 KiB".into()))
        }
        _ => Ok(()),
    }
}

pub(crate) fn tool(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > TOOL_BYTES || value.contains('\0') {
        return Err(ClixError::Usage(
            "tool must contain 1 to 4096 bytes and no NUL".into(),
        ));
    }
    Ok(())
}

pub(crate) fn result(job: &crate::types::Job) -> Result<()> {
    invocation(&job.id, &job.argv)?;
    status(&job.status)?;
    if job.stdout.len() as u64 > OUTPUT_STREAM_BYTES
        || job.stderr.len() as u64 > OUTPUT_STREAM_BYTES
    {
        return Err(ClixError::Protocol(
            "job output exceeds 4 MiB per stream".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn owner_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<String>> {
    limited_line(reader, OWNER_REQUEST_BYTES, IO_TIMEOUT).await
}

async fn limited_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
    deadline: Duration,
) -> std::io::Result<Option<String>> {
    use std::io::{Error, ErrorKind};
    let mut bytes = Vec::new();
    let count = tokio::time::timeout(
        deadline,
        reader
            .take((limit + 1) as u64)
            .read_until(b'\n', &mut bytes),
    )
    .await
    .map_err(|_| Error::new(ErrorKind::TimedOut, "owner request deadline exceeded"))??;
    if count == 0 {
        return Ok(None);
    }
    if bytes.len() > limit {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "owner request exceeds 128 KiB",
        ));
    }
    if bytes.last() != Some(&b'\n') {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            "incomplete owner request",
        ));
    }
    bytes.pop();
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| Error::new(ErrorKind::InvalidData, "owner request is not UTF-8"))
}

#[cfg(test)]
mod resource_tests {
    use super::*;

    #[tokio::test]
    async fn bounded_owner_read_rejects_long_and_idle_clients() {
        let mut long = &b"123456789\n"[..];
        assert_eq!(
            limited_line(&mut long, 8, IO_TIMEOUT)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        let mut two = &b"one\ntwo\n"[..];
        assert_eq!(
            limited_line(&mut two, 8, IO_TIMEOUT)
                .await
                .unwrap()
                .as_deref(),
            Some("one")
        );
        assert_eq!(
            limited_line(&mut two, 8, IO_TIMEOUT)
                .await
                .unwrap()
                .as_deref(),
            Some("two")
        );
        let (idle, _writer) = tokio::io::duplex(8);
        let mut idle = tokio::io::BufReader::new(idle);
        assert_eq!(
            limited_line(&mut idle, 8, Duration::from_millis(20))
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
    }
}
