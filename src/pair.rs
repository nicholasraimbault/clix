use std::fs;
use std::io::Read;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use hmac::{Hmac, Mac};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
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
const ID_CTX: &[u8] = b"clix-pair-v2";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if !name
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        || name.len() > 63
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(ClixError::Usage("body name must start with a letter or digit and contain 1–63 letters, digits, hyphens or underscores".into()));
    }
    Ok(())
}

pub(crate) fn validate_store(store: &Store) -> Result<()> {
    validate_name(&store.body_name)?;
    let local_pk = owner_pk(&store.owner_sk)?;
    let mut names = std::collections::HashSet::new();
    let mut keys = std::collections::HashSet::new();
    for p in &store.peers {
        validate_name(&p.name.0)?;
        let bytes: [u8; 32] = p
            .owner_pk
            .as_slice()
            .try_into()
            .map_err(|_| ClixError::Usage("invalid paired identity".into()))?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&bytes)
            .map_err(|_| ClixError::Usage("invalid paired identity".into()))?;
        if key.is_weak()
            || p.name.0 == store.body_name
            || p.owner_pk == local_pk
            || !names.insert(&p.name)
            || !keys.insert(&p.owner_pk)
        {
            return Err(ClixError::Usage(
                "ambiguous paired names or identities; owner must resolve pairing state".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Confirmation {
    joiner: PairIdentity,
    listener: PairIdentity,
}

/// Pairing is not a distributed transaction. Track trust independently from
/// confirmation so a dropped acknowledgement never conceals a saved peer.
#[derive(Default)]
struct Progress {
    local_peer: Option<String>,
    local_saved: bool,
    remote_may_trust: bool,
}

impl Progress {
    fn trust_may_exist(&self) -> bool {
        self.local_peer.is_some() || self.remote_may_trust
    }

    fn report(self, result: Result<Peer>) -> Result<Peer> {
        result.map_err(|e| {
            let e = if matches!(e, ClixError::Unreachable) {
                ClixError::Io("pairing did not finish; check the phrase and connection".into())
            } else { e };
            let state = match self.local_peer {
                Some(name) if self.local_saved => format!("This machine now trusts {name}"),
                Some(name) => format!("Pairing state may have been saved for {name}"),
                None if self.remote_may_trust => {
                    "The other machine may already trust this machine".into()
                }
                None => return e,
            };
            ClixError::Usage(format!(
                "{state}, but pairing confirmation did not finish: {e}. Check clix status on both machines."
            ))
        })
    }
}

struct Attempt {
    result: Result<Peer>,
    trust_may_exist: bool,
}

fn pairing_timeout() -> ClixError {
    ClixError::Usage("pairing timed out; start a new phrase".into())
}

/// Wait for a joiner on `mesh`, run SPAKE2, persist the peer.
pub async fn pair_listen(
    store: Arc<Mutex<Store>>,
    mesh: &MeshHandle,
    phrase: &str,
) -> Result<Peer> {
    let rx = mesh.register_pair();
    complete_listen(store, rx, phrase, &mesh.addr()).await
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
    let mut progress = Progress::default();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        handshake_listen(store, stream, phrase, local_addr, &mut progress),
    )
    .await
    .unwrap_or_else(|_| Err(pairing_timeout()));
    progress.report(result)
}

async fn join_attempt(
    store: Arc<Mutex<Store>>,
    stream: TcpStream,
    phrase: &str,
    dial_addr: &str,
    local_addr: &str,
) -> Attempt {
    let mut progress = Progress::default();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        handshake_join(store, stream, phrase, dial_addr, local_addr, &mut progress),
    )
    .await
    .unwrap_or_else(|_| Err(pairing_timeout()));
    Attempt {
        trust_may_exist: progress.trust_may_exist(),
        result: progress.report(result),
    }
}

/// Dial `mesh` (addr), run SPAKE2, persist the peer, then synchronize pins.
pub async fn pair_join(
    store: Arc<Mutex<Store>>,
    mesh: &str,
    phrase: &str,
    local_addr: &str,
) -> Result<Peer> {
    let stream = mesh::dial(mesh).await?;
    let peer = join_attempt(store.clone(), stream, phrase, mesh, local_addr)
        .await
        .result?;
    pin_after_pair(&store, &peer).await?;
    Ok(peer)
}

/// Discovery is plumbing. Stop trying additional machines once trust was saved
/// or may have been saved, even if the last acknowledgement was lost.
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
        let attempt = join_attempt(store.clone(), stream, phrase, &addr, local_addr).await;
        match attempt.result {
            Ok(peer) => {
                pin_after_pair(&store, &peer).await?;
                return Ok(peer);
            }
            Err(e) if attempt.trust_may_exist => return Err(e),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| ClixError::Io("no online Tailscale peers".into())))
}

fn phrase_mismatch() -> ClixError {
    ClixError::Io("phrase did not match".into())
}

fn local_identity(store: &Arc<Mutex<Store>>, addr: &str) -> Result<PairIdentity> {
    let s = store.lock().unwrap_or_else(|e| e.into_inner());
    validate_name(&s.body_name)?;
    Ok(PairIdentity {
        name: s.body_name.clone(),
        owner_pk: owner_pk(&s.owner_sk)?,
        addr: Some(addr.to_string()),
    })
}

fn peer_from(identity: &PairIdentity, fallback: Option<String>) -> Peer {
    Peer {
        name: BodyId(identity.name.clone()),
        owner_pk: identity.owner_pk.clone(),
        addr: identity.addr.clone().filter(|s| !s.is_empty()).or(fallback),
    }
}

fn update_peer(s: &mut Store, peer: &Peer) -> Result<()> {
    if let Some(existing) = s
        .peers
        .iter_mut()
        .find(|p| p.name == peer.name || p.owner_pk == peer.owner_pk)
    {
        if existing.name != peer.name || existing.owner_pk != peer.owner_pk {
            return Err(ClixError::Usage(
                "that name or identity is already paired; refusing to replace it".into(),
            ));
        }
        existing.addr = peer.addr.clone();
    } else {
        s.peers.push(peer.clone());
    }
    validate_store(s)
}

fn validate_peer(store: &Arc<Mutex<Store>>, peer: &Peer) -> Result<()> {
    let mut candidate = store.lock().unwrap_or_else(|e| e.into_inner()).clone();
    update_peer(&mut candidate, peer)
}

fn persist_peer(
    store: &Arc<Mutex<Store>>,
    local: &PairIdentity,
    peer: &Peer,
    progress: &mut Progress,
) -> Result<()> {
    let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
    if s.body_name != local.name || owner_pk(&s.owner_sk)? != local.owner_pk {
        return Err(ClixError::Usage(
            "local identity changed during pairing; start a new phrase".into(),
        ));
    }
    // A failed durable write can leave the new file on disk (for example if
    // directory fsync fails); conservatively report that possible trust too.
    progress.local_peer = Some(peer.name.0.clone());
    s.update(|s| update_peer(s, peer))?;
    progress.local_saved = true;
    Ok(())
}

async fn handshake_listen(
    store: Arc<Mutex<Store>>,
    mut stream: TcpStream,
    phrase: &str,
    local_addr: &str,
    progress: &mut Progress,
) -> Result<Peer> {
    let local = local_identity(&store, local_addr)?;
    let (spake, msg_b) = Spake2::<Ed25519Group>::start_b(
        &Password::new(phrase.as_bytes()),
        &Identity::new(ID_A),
        &Identity::new(ID_B),
    );
    let msg_a = mesh::read_frame(&mut stream).await?;
    mesh::write_frame(&mut stream, &msg_b).await?;
    let key = spake.finish(&msg_a).map_err(|_| phrase_mismatch())?;
    let remote: PairIdentity = read_authenticated(&mut stream, &key, b"join-identity").await?;
    let peer = peer_from(&remote, stream.peer_addr().ok().map(|a| a.to_string()));
    validate_peer(&store, &peer)?;
    // Receiving this identity lets the joiner save us. Subsequent errors must
    // not claim that the other machine necessarily remains unpaired.
    progress.remote_may_trust = true;
    write_authenticated(&mut stream, &key, b"listen-identity", &local).await?;
    let expected = Confirmation {
        joiner: remote,
        listener: local.clone(),
    };
    let confirmed: Confirmation = read_authenticated(&mut stream, &key, b"join-committed").await?;
    if confirmed != expected {
        return Err(phrase_mismatch());
    }
    persist_peer(&store, &local, &peer, progress)?;
    write_authenticated(&mut stream, &key, b"listen-committed", &expected).await?;
    Ok(peer)
}

async fn handshake_join(
    store: Arc<Mutex<Store>>,
    mut stream: TcpStream,
    phrase: &str,
    dial_addr: &str,
    local_addr: &str,
    progress: &mut Progress,
) -> Result<Peer> {
    let local = local_identity(&store, local_addr)?;
    let (spake, msg_a) = Spake2::<Ed25519Group>::start_a(
        &Password::new(phrase.as_bytes()),
        &Identity::new(ID_A),
        &Identity::new(ID_B),
    );
    mesh::write_frame(&mut stream, &msg_a).await?;
    let msg_b = mesh::read_frame(&mut stream).await?;
    let key = spake.finish(&msg_b).map_err(|_| phrase_mismatch())?;
    write_authenticated(&mut stream, &key, b"join-identity", &local).await?;
    let remote: PairIdentity = read_authenticated(&mut stream, &key, b"listen-identity").await?;
    let peer = peer_from(&remote, Some(dial_addr.to_string()));
    validate_peer(&store, &peer)?;
    persist_peer(&store, &local, &peer, progress)?;
    let expected = Confirmation {
        joiner: local,
        listener: remote,
    };
    progress.remote_may_trust = true;
    write_authenticated(&mut stream, &key, b"join-committed", &expected).await?;
    let confirmed: Confirmation =
        read_authenticated(&mut stream, &key, b"listen-committed").await?;
    if confirmed != expected {
        return Err(phrase_mismatch());
    }
    Ok(peer)
}

/// Trust has already been established. A pin conflict does not undo pairing.
pub(crate) async fn pin_after_pair(store: &Arc<Mutex<Store>>, peer: &Peer) -> Result<()> {
    let sk = store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .owner_sk
        .clone();
    crate::pin::sync_after_pair(store, &sk, peer)
        .await
        .map_err(|e| {
            ClixError::Usage(format!(
                "Paired with {}; pin synchronization did not finish: {e}. Pairing remains saved.",
                peer.name
            ))
        })
}

async fn write_authenticated<T: Serialize>(
    stream: &mut TcpStream,
    key: &[u8],
    phase: &[u8],
    message: &T,
) -> Result<()> {
    let payload = serde_json::to_vec(message)?;
    let mut mac =
        HmacSha256::new_from_slice(key).map_err(|_| ClixError::Io("pair hmac key".into()))?;
    mac.update(ID_CTX);
    mac.update(&[0]);
    mac.update(phase);
    mac.update(&[0]);
    mac.update(&payload);
    let mut frame = Vec::with_capacity(32 + payload.len());
    frame.extend_from_slice(&mac.finalize().into_bytes());
    frame.extend_from_slice(&payload);
    mesh::write_frame(stream, &frame).await
}

async fn read_authenticated<T: DeserializeOwned>(
    stream: &mut TcpStream,
    key: &[u8],
    phase: &[u8],
) -> Result<T> {
    let frame = mesh::read_frame(stream).await?;
    if frame.len() < 32 {
        return Err(phrase_mismatch());
    }
    let (tag, payload) = frame.split_at(32);
    let mut mac =
        HmacSha256::new_from_slice(key).map_err(|_| ClixError::Io("pair hmac key".into()))?;
    mac.update(ID_CTX);
    mac.update(&[0]);
    mac.update(phase);
    mac.update(&[0]);
    mac.update(payload);
    mac.verify_slice(tag).map_err(|_| phrase_mismatch())?;
    serde_json::from_slice(payload).map_err(|_| phrase_mismatch())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn test_store(name: &str, seed: u8) -> (tempfile::TempDir, Arc<Mutex<Store>>) {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path()).unwrap();
        s.body_name = name.into();
        s.owner_sk = vec![seed; 32];
        (dir, Arc::new(Mutex::new(s)))
    }

    #[tokio::test]
    async fn rejected_listener_identity_does_not_authorize_joiner_on_listener() {
        let (_a_dir, a) = test_store("alpha", 1);
        let (_b_dir, b) = test_store("bravo", 2);
        b.lock().unwrap().peers.push(Peer {
            name: BodyId("alpha".into()),
            owner_pk: owner_pk(&[3; 32]).unwrap(),
            addr: None,
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let a_run = a.clone();
        let a_addr = addr.clone();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut progress = Progress::default();
            let result = handshake_listen(a_run, stream, "phrase", &a_addr, &mut progress).await;
            assert!(result.is_err());
        });
        let stream = TcpStream::connect(&addr).await.unwrap();
        let attempt = join_attempt(b.clone(), stream, "phrase", &addr, "127.0.0.1:9").await;
        assert!(attempt.result.is_err());
        assert!(!attempt.trust_may_exist);
        task.await.unwrap();
        assert!(a.lock().unwrap().peers.is_empty());
        assert_eq!(b.lock().unwrap().peers.len(), 1);
    }

    #[tokio::test]
    async fn replaying_join_identity_as_confirmation_does_not_authorize_peer() {
        let (_dir, store) = test_store("alpha", 1);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let serving = store.clone();
        let local_addr = addr.clone();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut progress = Progress::default();
            assert!(
                handshake_listen(serving, stream, "phrase", &local_addr, &mut progress)
                    .await
                    .is_err()
            );
        });
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let (spake, msg) = Spake2::<Ed25519Group>::start_a(
            &Password::new(b"phrase"),
            &Identity::new(ID_A),
            &Identity::new(ID_B),
        );
        mesh::write_frame(&mut stream, &msg).await.unwrap();
        let key = spake
            .finish(&mesh::read_frame(&mut stream).await.unwrap())
            .unwrap();
        let identity = PairIdentity {
            name: "bravo".into(),
            owner_pk: owner_pk(&[2; 32]).unwrap(),
            addr: None,
        };
        write_authenticated(&mut stream, &key, b"join-identity", &identity)
            .await
            .unwrap();
        let _: PairIdentity = read_authenticated(&mut stream, &key, b"listen-identity")
            .await
            .unwrap();
        write_authenticated(&mut stream, &key, b"join-identity", &identity)
            .await
            .unwrap();
        task.await.unwrap();
        assert!(store.lock().unwrap().peers.is_empty());
    }

    #[tokio::test]
    async fn lost_final_acknowledgement_reports_saved_local_trust() {
        let (_a_dir, a) = test_store("alpha", 1);
        let (_b_dir, b) = test_store("bravo", 2);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let local_addr = addr.clone();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (spake, msg) = Spake2::<Ed25519Group>::start_b(
                &Password::new(b"phrase"),
                &Identity::new(ID_A),
                &Identity::new(ID_B),
            );
            let incoming = mesh::read_frame(&mut stream).await.unwrap();
            mesh::write_frame(&mut stream, &msg).await.unwrap();
            let key = spake.finish(&incoming).unwrap();
            let _: PairIdentity = read_authenticated(&mut stream, &key, b"join-identity")
                .await
                .unwrap();
            write_authenticated(
                &mut stream,
                &key,
                b"listen-identity",
                &local_identity(&a, &local_addr).unwrap(),
            )
            .await
            .unwrap();
            let _: Confirmation = read_authenticated(&mut stream, &key, b"join-committed")
                .await
                .unwrap();
            // Connection disappears before the final listener acknowledgement.
        });
        let stream = TcpStream::connect(&addr).await.unwrap();
        let attempt = join_attempt(b.clone(), stream, "phrase", &addr, "127.0.0.1:9").await;
        assert!(attempt.trust_may_exist);
        let message = attempt.result.unwrap_err().to_string();
        assert!(
            message.contains("This machine now trusts alpha"),
            "{message}"
        );
        assert_eq!(b.lock().unwrap().peers.len(), 1);
        task.await.unwrap();
    }

    #[test]
    fn body_names_must_be_addressable_as_cli_destinations() {
        assert!(validate_name("-laptop").is_err());
        assert!(validate_name("laptop-1").is_ok());
    }
}
