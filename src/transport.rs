//! TLS 1.3 authenticated by the Ed25519 identities established during pairing.
//!
//! RFC 7250 raw public keys avoid a second identity or certificate lifecycle.
//! Rustls verifies handshake signatures and encrypts both directions. Clix's
//! verifier only decides which already-paired public key is acceptable.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::pkcs8::{DecodePublicKey, EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::{SigningKey, VerifyingKey};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{AlwaysResolvesClientRawPublicKeys, Resumption};
use rustls::crypto::{verify_tls13_signature_with_raw_key, CryptoProvider};
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, SubjectPublicKeyInfoDer,
    UnixTime,
};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::{AlwaysResolvesServerRawPublicKeys, NoServerSessionStorage};
use rustls::sign::CertifiedKey;
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme,
};
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::error::{ClixError, Result};
use crate::store::Store;
use crate::types::Peer;

const ALPN: &[u8] = b"clix/1";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

fn provider() -> CryptoProvider {
    rustls::crypto::ring::default_provider()
}

fn tls_error(error: impl fmt::Display) -> ClixError {
    ClixError::Io(format!("mesh authentication failed: {error}"))
}

fn unknown_peer() -> rustls::Error {
    rustls::Error::General("unknown paired identity".into())
}

fn public_key_der(pk: &[u8]) -> Result<Vec<u8>> {
    let bytes: [u8; 32] = pk
        .try_into()
        .map_err(|_| tls_error("invalid Ed25519 public key length"))?;
    let key = VerifyingKey::from_bytes(&bytes).map_err(tls_error)?;
    if key.is_weak() {
        return Err(tls_error("weak Ed25519 public key"));
    }
    Ok(key
        .to_public_key_der()
        .map_err(tls_error)?
        .as_bytes()
        .to_vec())
}

fn identity(sk: &[u8]) -> Result<Arc<CertifiedKey>> {
    let bytes: [u8; 32] = sk
        .try_into()
        .map_err(|_| tls_error("invalid Ed25519 secret key length"))?;
    let doc = SigningKey::from_bytes(&bytes)
        .to_pkcs8_der()
        .map_err(tls_error)?;
    let key = provider()
        .key_provider
        .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            doc.as_bytes().to_vec(),
        )))
        .map_err(tls_error)?;
    let spki = key
        .public_key()
        .ok_or_else(|| tls_error("identity has no public key"))?;
    Ok(Arc::new(CertifiedKey::new(
        vec![CertificateDer::from(spki.as_ref().to_vec())],
        key,
    )))
}

fn verify_signature(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
    if dss.scheme != SignatureScheme::ED25519 {
        return Err(rustls::Error::General("Ed25519 signature required".into()));
    }
    verify_tls13_signature_with_raw_key(
        message,
        &SubjectPublicKeyInfoDer::from(cert.as_ref()),
        dss,
        &provider().signature_verification_algorithms,
    )
}

fn reject_tls12() -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
    Err(rustls::Error::General("TLS 1.3 required".into()))
}

#[derive(Debug)]
struct ExpectedServer {
    spki: Vec<u8>,
}

impl ServerCertVerifier for ExpectedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        // Device identity is the paired key, independent of its address or DNS.
        if !intermediates.is_empty() || end_entity.as_ref() != self.spki.as_slice() {
            return Err(unknown_peer());
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        reject_tls12()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

struct PairedClients {
    store: Arc<Mutex<Store>>,
}

impl fmt::Debug for PairedClients {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Store contains secret material; do not expose it in TLS diagnostics.
        f.debug_struct("PairedClients").finish_non_exhaustive()
    }
}

impl ClientCertVerifier for PairedClients {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        if !intermediates.is_empty() {
            return Err(unknown_peer());
        }
        let store = self.store.lock().map_err(|_| unknown_peer())?;
        let paired = store.peers.iter().any(|peer| {
            public_key_der(&peer.owner_pk).is_ok_and(|spki| spki.as_slice() == end_entity.as_ref())
        });
        if !paired {
            return Err(unknown_peer());
        }
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        reject_tls12()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

pub fn client_config(sk: &[u8], expected_pk: &[u8]) -> Result<Arc<ClientConfig>> {
    let verifier = ExpectedServer {
        spki: public_key_der(expected_pk)?,
    };
    let mut config = ClientConfig::builder_with_provider(provider().into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_client_cert_resolver(Arc::new(AlwaysResolvesClientRawPublicKeys::new(identity(
            sk,
        )?)));
    config.alpn_protocols = vec![ALPN.to_vec()];
    config.enable_sni = false;
    config.enable_early_data = false;
    config.resumption = Resumption::disabled();
    Ok(Arc::new(config))
}

pub fn server_config(store: Arc<Mutex<Store>>) -> Result<Arc<ServerConfig>> {
    let key = {
        let store = store
            .lock()
            .map_err(|_| tls_error("identity store unavailable"))?;
        identity(&store.owner_sk)?
    };
    let mut config = ServerConfig::builder_with_provider(provider().into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_error)?
        .with_client_cert_verifier(Arc::new(PairedClients { store }))
        .with_cert_resolver(Arc::new(AlwaysResolvesServerRawPublicKeys::new(key)));
    config.alpn_protocols = vec![ALPN.to_vec()];
    config.max_early_data_size = 0;
    config.send_tls13_tickets = 0;
    config.session_storage = Arc::new(NoServerSessionStorage {});
    Ok(Arc::new(config))
}

/// Decode only an identity returned by a completed, verified TLS handshake.
pub fn peer_pk(certs: Option<&[CertificateDer<'_>]>) -> Result<Vec<u8>> {
    let Some([cert]) = certs else {
        return Err(tls_error("missing or ambiguous peer identity"));
    };
    Ok(VerifyingKey::from_public_key_der(cert.as_ref())
        .map_err(tls_error)?
        .to_bytes()
        .to_vec())
}

/// Authenticate an already-connected socket. TCP reachability is handled by
/// the caller, separately from identity rejection.
pub async fn connect(
    stream: TcpStream,
    sk: &[u8],
    expected_pk: &[u8],
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    // Rustls requires a syntactic ServerName; the paired key, never this name,
    // is the trust decision. SNI is disabled in client_config.
    let name = ServerName::try_from("clix.invalid").map_err(tls_error)?;
    let stream = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        TlsConnector::from(client_config(sk, expected_pk)?).connect(name, stream),
    )
    .await
    .map_err(|_| ClixError::Unreachable)?
    .map_err(crate::mesh::network_error)?;
    if stream.get_ref().1.alpn_protocol() != Some(ALPN) {
        return Err(tls_error("Clix protocol was not negotiated"));
    }
    Ok(stream)
}

/// Require a currently-paired client identity before exposing a request.
pub async fn accept(
    stream: TcpStream,
    config: Arc<ServerConfig>,
    store: &Arc<Mutex<Store>>,
) -> Result<(tokio_rustls::server::TlsStream<TcpStream>, Peer)> {
    let stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, TlsAcceptor::from(config).accept(stream))
        .await
        .map_err(|_| tls_error("handshake timed out"))?
        .map_err(tls_error)?;
    if stream.get_ref().1.alpn_protocol() != Some(ALPN) {
        return Err(tls_error("Clix protocol was not negotiated"));
    }
    let pk = peer_pk(stream.get_ref().1.peer_certificates())?;
    let peer = store
        .lock()
        .map_err(|_| tls_error("identity store unavailable"))?
        .peers
        .iter()
        .find(|peer| peer.owner_pk == pk)
        .cloned()
        .ok_or_else(|| tls_error("unknown paired identity"))?;
    Ok((stream, peer))
}

#[cfg(test)]
mod tests {
    use crate::{store::Store, transport, types::Peer};
    use ed25519_dalek::SigningKey;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    fn pk(seed: u8) -> Vec<u8> {
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes()
            .to_vec()
    }
    fn store(allow_client: bool) -> Arc<Mutex<Store>> {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path()).unwrap();
        s.owner_sk = vec![1; 32];
        s.peers = if allow_client {
            vec![Peer {
                name: crate::types::BodyId("client".into()),
                owner_pk: pk(2),
                addr: None,
            }]
        } else {
            vec![]
        };
        Arc::new(Mutex::new(s))
    }

    #[tokio::test]
    async fn mutual_auth_preserves_binary_request_and_response() {
        let store = store(true);
        let config = transport::server_config(store.clone()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut tls, peer) = transport::accept(socket, config, &store).await.unwrap();
            assert_eq!(peer.owner_pk, pk(2));
            let mut bytes = [0; 3];
            tls.read_exact(&mut bytes).await.unwrap();
            assert_eq!(bytes, [0, 255, 128]);
            tls.write_all(&[128, 0, 255]).await.unwrap();
            tls.flush().await.unwrap();
        });
        let socket = TcpStream::connect(addr).await.unwrap();
        let mut tls = transport::connect(socket, &[2; 32], &pk(1)).await.unwrap();
        assert_eq!(
            transport::peer_pk(tls.get_ref().1.peer_certificates()).unwrap(),
            pk(1)
        );
        tls.write_all(&[0, 255, 128]).await.unwrap();
        tls.flush().await.unwrap();
        let mut bytes = [0; 3];
        tls.read_exact(&mut bytes).await.unwrap();
        assert_eq!(bytes, [128, 0, 255]);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_different_server_identity_is_rejected_before_request() {
        let store = store(true);
        let config = transport::server_config(store.clone()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            assert!(transport::accept(socket, config, &store).await.is_err());
        });
        let socket = TcpStream::connect(addr).await.unwrap();
        assert!(transport::connect(socket, &[2; 32], &pk(3)).await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn unpaired_client_gets_no_application_response() {
        let store = store(false);
        let config = transport::server_config(store.clone()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            assert!(transport::accept(socket, config, &store).await.is_err());
        });
        let socket = TcpStream::connect(addr).await.unwrap();
        if let Ok(mut tls) = transport::connect(socket, &[2; 32], &pk(1)).await {
            let mut bytes = [0; 1];
            assert!(tls.read_exact(&mut bytes).await.is_err());
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn live_allowlist_updates_affect_existing_server_config() {
        let store = store(false);
        let config = transport::server_config(store.clone()).unwrap();
        store.lock().unwrap().peers.push(Peer {
            name: crate::types::BodyId("client".into()),
            owner_pk: pk(2),
            addr: None,
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut tls, _) = transport::accept(socket, config, &store).await.unwrap();
            tls.write_all(b"accepted").await.unwrap();
            tls.flush().await.unwrap();
        });
        let socket = TcpStream::connect(addr).await.unwrap();
        let mut tls = transport::connect(socket, &[2; 32], &pk(1)).await.unwrap();
        let mut bytes = [0; 8];
        tls.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"accepted");
        server.await.unwrap();
    }

    fn forged_identity(claimed_seed: u8, actual_seed: u8) -> Arc<rustls::sign::CertifiedKey> {
        use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let secret = SigningKey::from_bytes(&[actual_seed; 32])
            .to_pkcs8_der()
            .unwrap();
        let key = rustls::crypto::ring::default_provider()
            .key_provider
            .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                secret.as_bytes().to_vec(),
            )))
            .unwrap();
        let claimed = SigningKey::from_bytes(&[claimed_seed; 32])
            .verifying_key()
            .to_public_key_der()
            .unwrap();
        Arc::new(rustls::sign::CertifiedKey::new(
            vec![CertificateDer::from(claimed.as_bytes().to_vec())],
            key,
        ))
    }

    #[tokio::test]
    async fn presenting_paired_client_public_key_without_its_secret_is_rejected() {
        let store = store(true);
        let config = transport::server_config(store.clone()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            assert!(transport::accept(socket, config, &store).await.is_err());
        });
        let mut config = transport::client_config(&[2; 32], &pk(1)).unwrap();
        Arc::get_mut(&mut config).unwrap().client_auth_cert_resolver = Arc::new(
            rustls::client::AlwaysResolvesClientRawPublicKeys::new(forged_identity(2, 3)),
        );
        let socket = TcpStream::connect(addr).await.unwrap();
        let result = tokio_rustls::TlsConnector::from(config)
            .connect(
                rustls::pki_types::ServerName::try_from("clix.invalid").unwrap(),
                socket,
            )
            .await;
        if let Ok(mut tls) = result {
            assert!(tls.read_exact(&mut [0; 1]).await.is_err());
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn presenting_expected_server_public_key_without_its_secret_is_rejected() {
        let store = store(true);
        let mut config = transport::server_config(store.clone()).unwrap();
        Arc::get_mut(&mut config).unwrap().cert_resolver = Arc::new(
            rustls::server::AlwaysResolvesServerRawPublicKeys::new(forged_identity(1, 3)),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            assert!(transport::accept(socket, config, &store).await.is_err());
        });
        let socket = TcpStream::connect(addr).await.unwrap();
        assert!(transport::connect(socket, &[2; 32], &pk(1)).await.is_err());
        server.await.unwrap();
    }
}
