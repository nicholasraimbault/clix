use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::error::{ClixError, Result};
use crate::types::Peer;

/// Shared handle for pairing and (later) mesh exec.
#[derive(Clone)]
pub struct MeshHandle {
    pub addr: String,
    inner: Arc<Mutex<MeshInner>>,
}

struct MeshInner {
    pair_tx: Option<oneshot::Sender<TcpStream>>,
    pair_done: Option<oneshot::Receiver<Result<Peer>>>,
}

/// Localhost TCP listener. Production Tailscale bind is Task 13.
pub struct MeshListener {
    listener: TcpListener,
    handle: MeshHandle,
}

impl MeshListener {
    pub async fn bind(addr: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        Ok(Self {
            listener,
            handle: MeshHandle {
                addr: local.to_string(),
                inner: Arc::new(Mutex::new(MeshInner {
                    pair_tx: None,
                    pair_done: None,
                })),
            },
        })
    }

    pub fn handle(&self) -> MeshHandle {
        self.handle.clone()
    }

    pub fn local_addr(&self) -> &str {
        &self.handle.addr
    }

    pub async fn run(&self) -> Result<()> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            let waiter = {
                let mut inner = lock_inner(&self.handle);
                inner.pair_tx.take()
            };
            if let Some(tx) = waiter {
                let _ = tx.send(stream);
            }
        }
    }
}

impl MeshHandle {
    /// Register so the next accepted TCP stream is delivered for pairing.
    pub fn register_pair(&self) -> oneshot::Receiver<TcpStream> {
        let (tx, rx) = oneshot::channel();
        lock_inner(self).pair_tx = Some(tx);
        rx
    }

    pub fn set_pair_done(&self, rx: oneshot::Receiver<Result<Peer>>) {
        lock_inner(self).pair_done = Some(rx);
    }

    pub fn take_pair_done(&self) -> Result<oneshot::Receiver<Result<Peer>>> {
        lock_inner(self)
            .pair_done
            .take()
            .ok_or_else(|| ClixError::Usage("not pairing".into()))
    }
}

fn lock_inner(handle: &MeshHandle) -> std::sync::MutexGuard<'_, MeshInner> {
    handle.inner.lock().unwrap_or_else(|e| e.into_inner())
}

pub async fn dial(addr: &str) -> Result<TcpStream> {
    Ok(TcpStream::connect(addr).await?)
}

pub async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> Result<()> {
    let len = u32::try_from(data.len()).map_err(|_| ClixError::Io("frame too large".into()))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

pub async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut lenb = [0u8; 4];
    stream.read_exact(&mut lenb).await?;
    let len = u32::from_be_bytes(lenb) as usize;
    if len > 1_048_576 {
        return Err(ClixError::Io("frame too large".into()));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}
