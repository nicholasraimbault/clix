use std::fs;
use std::io::Read;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

use crate::error::{ClixError, Result};
use crate::mesh::{self, MeshHandle};
use crate::store::Store;
use crate::types::{BodyId, Peer};

const WORDS: &str = include_str!("pair_words.txt");
const ID_A: &[u8] = b"clix-a";
const ID_B: &[u8] = b"clix-b";
const ID_CTX: &[u8] = b"clix-pair-v1";

type HmacSha256 = Hmac<Sha256>;

#[derive(Serialize, Deserialize)]
struct PairIdentity {
    name: String,
    owner_pk: Vec<u8>,
    #[serde(default)]
    addr: Option<String>,
}

/// Three lowercase words from the 256-word list, joined with `-`.
pub fn phrase() -> String {
    let words = word_list();
    debug_assert_eq!(words.len(), 256);
    let buf = random_bytes(3).expect("urandom");
    format!(
        "{}-{}-{}",
        words[buf[0] as usize], words[buf[1] as usize], words[buf[2] as usize]
    )
}

fn word_list() -> Vec<&'static str> {
    WORDS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
}

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    let mut f = fs::File::open("/dev/urandom")
        .map_err(|e| ClixError::Io(format!("read /dev/urandom: {e}")))?;
    f.read_exact(&mut buf)
        .map_err(|e| ClixError::Io(format!("read /dev/urandom: {e}")))?;
    Ok(buf)
}

/// Owner ed25519 public key from the 32-byte seed in the store.
pub fn owner_pk(sk: &[u8]) -> Result<Vec<u8>> {
    let bytes: [u8; 32] = sk
        .try_into()
        .map_err(|_| ClixError::Io("owner key is not a valid ed25519 secret".into()))?;
    let sk = SigningKey::from_bytes(&bytes);
    Ok(sk.verifying_key().to_bytes().to_vec())
}

pub(crate) fn hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "clix".into())
}

/// Wait for a joiner on `mesh`, run SPAKE2, persist the peer.
pub async fn pair_listen(
    store: Arc<Mutex<Store>>,
    mesh: &MeshHandle,
    phrase: &str,
) -> Result<Peer> {
    let rx = mesh.register_pair();
    complete_listen(store, rx, phrase, &mesh.addr).await
}

pub(crate) async fn complete_listen(
    store: Arc<Mutex<Store>>,
    rx: oneshot::Receiver<TcpStream>,
    phrase: &str,
    local_addr: &str,
) -> Result<Peer> {
    let stream = rx
        .await
        .map_err(|_| ClixError::Io("pair cancelled".into()))?;
    handshake_listen(store, stream, phrase, local_addr).await
}

/// Dial `mesh` (addr), run SPAKE2, persist the peer.
pub async fn pair_join(
    store: Arc<Mutex<Store>>,
    mesh: &str,
    phrase: &str,
    local_addr: &str,
) -> Result<Peer> {
    let stream = mesh::dial(mesh).await?;
    handshake_join(store, stream, phrase, mesh, local_addr).await
}

/// Join with no addr: try each online Tailscale peer. Tailscale is plumbing, not identity.
pub async fn pair_join_any(
    store: Arc<Mutex<Store>>,
    phrase: &str,
    local_addr: &str,
) -> Result<Peer> {
    let peers = crate::tailscale::tailscale_status()?;
    let port = crate::tailscale::mesh_port();
    let mut last = None;
    for peer in &peers {
        let addr = format!("{}:{port}", peer.ipv4);
        let dial = tokio::time::timeout(std::time::Duration::from_secs(2), mesh::dial(&addr)).await;
        let stream = match dial {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                last = Some(e);
                continue;
            }
            Err(_) => {
                last = Some(ClixError::Io("no peer answered".into()));
                continue;
            }
        };
        match handshake_join(store.clone(), stream, phrase, &addr, local_addr).await {
            Ok(p) => return Ok(p),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| ClixError::Io("no online Tailscale peers".into())))
}

fn phrase_mismatch() -> ClixError {
    ClixError::Io("phrase did not match".into())
}

fn local_identity(store: &Arc<Mutex<Store>>) -> Result<(String, Vec<u8>)> {
    let s = store.lock().unwrap_or_else(|e| e.into_inner());
    let name = if s.body_name.is_empty() {
        hostname()
    } else {
        s.body_name.clone()
    };
    let pk = owner_pk(&s.owner_sk)?;
    Ok((name, pk))
}

fn persist_peer(store: &Arc<Mutex<Store>>, peer: Peer) -> Result<Peer> {
    let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = s.peers.iter_mut().find(|p| p.name == peer.name) {
        *existing = peer.clone();
    } else {
        s.peers.push(peer.clone());
    }
    s.save()?;
    Ok(peer)
}

async fn handshake_listen(
    store: Arc<Mutex<Store>>,
    mut stream: TcpStream,
    phrase: &str,
    local_addr: &str,
) -> Result<Peer> {
    let (name, pk) = local_identity(&store)?;
    let (spake, msg_b) = Spake2::<Ed25519Group>::start_b(
        &Password::new(phrase.as_bytes()),
        &Identity::new(ID_A),
        &Identity::new(ID_B),
    );
    let msg_a = mesh::read_frame(&mut stream)
        .await
        .map_err(|_| phrase_mismatch())?;
    mesh::write_frame(&mut stream, &msg_b)
        .await
        .map_err(|_| phrase_mismatch())?;
    let key = spake.finish(&msg_a).map_err(|_| phrase_mismatch())?;
    let peer_id = read_identity(&mut stream, &key).await?;
    let peer = persist_peer(
        &store,
        Peer {
            name: BodyId(peer_id.name),
            owner_pk: peer_id.owner_pk,
            addr: peer_id
                .addr
                .filter(|s| !s.is_empty())
                .or_else(|| stream.peer_addr().ok().map(|a| a.to_string())),
        },
    )?;
    write_identity(&mut stream, &key, &name, &pk, local_addr).await?;
    Ok(peer)
}

async fn handshake_join(
    store: Arc<Mutex<Store>>,
    mut stream: TcpStream,
    phrase: &str,
    dial_addr: &str,
    local_addr: &str,
) -> Result<Peer> {
    let (name, pk) = local_identity(&store)?;
    let (spake, msg_a) = Spake2::<Ed25519Group>::start_a(
        &Password::new(phrase.as_bytes()),
        &Identity::new(ID_A),
        &Identity::new(ID_B),
    );
    mesh::write_frame(&mut stream, &msg_a)
        .await
        .map_err(|_| phrase_mismatch())?;
    let msg_b = mesh::read_frame(&mut stream)
        .await
        .map_err(|_| phrase_mismatch())?;
    let key = spake.finish(&msg_b).map_err(|_| phrase_mismatch())?;
    write_identity(&mut stream, &key, &name, &pk, local_addr).await?;
    let peer_id = read_identity(&mut stream, &key).await?;
    persist_peer(
        &store,
        Peer {
            name: BodyId(peer_id.name),
            owner_pk: peer_id.owner_pk,
            addr: peer_id
                .addr
                .filter(|s| !s.is_empty())
                .or_else(|| Some(dial_addr.to_string())),
        },
    )
}

async fn write_identity(
    stream: &mut TcpStream,
    key: &[u8],
    name: &str,
    owner_pk: &[u8],
    addr: &str,
) -> Result<()> {
    let payload = serde_json::to_vec(&PairIdentity {
        name: name.to_string(),
        owner_pk: owner_pk.to_vec(),
        addr: Some(addr.to_string()),
    })?;
    let mut mac =
        HmacSha256::new_from_slice(key).map_err(|_| ClixError::Io("pair hmac key".into()))?;
    mac.update(ID_CTX);
    mac.update(&payload);
    let tag = mac.finalize().into_bytes();
    let mut frame = Vec::with_capacity(32 + payload.len());
    frame.extend_from_slice(&tag);
    frame.extend_from_slice(&payload);
    mesh::write_frame(stream, &frame)
        .await
        .map_err(|_| phrase_mismatch())
}

async fn read_identity(stream: &mut TcpStream, key: &[u8]) -> Result<PairIdentity> {
    let frame = mesh::read_frame(stream)
        .await
        .map_err(|_| phrase_mismatch())?;
    if frame.len() < 32 {
        return Err(phrase_mismatch());
    }
    let (tag, payload) = frame.split_at(32);
    let mut mac =
        HmacSha256::new_from_slice(key).map_err(|_| ClixError::Io("pair hmac key".into()))?;
    mac.update(ID_CTX);
    mac.update(payload);
    mac.verify_slice(tag).map_err(|_| phrase_mismatch())?;
    serde_json::from_slice(payload).map_err(|_| phrase_mismatch())
}
