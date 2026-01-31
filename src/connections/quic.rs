//! Configuration for QUIC connections to remote peers
use anyhow::anyhow;
use harddrive_party_shared::ui_messages::UiServerError;
use log::{debug, warn};
use quinn::{
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
    AsyncUdpSocket, ClientConfig, Connection, Endpoint, ServerConfig,
};
use ring::signature::Ed25519KeyPair;
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    SignatureScheme,
};
use std::{sync::Arc, time::Duration};

use crate::{connections::certificate_to_name, errors::UiServerErrorWrapper};

use super::known_peers::KnownPeers;

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// This makes it possible to add breaking protocol changes and provide backwards compatibility
const SUPPORTED_PROTOCOL_VERSIONS: [&[u8]; 1] = [b"harddrive-party-v0"];

/// Generate a TLS certificate with Ed25519 keypair
pub fn generate_certificate() -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let mut cert_params = rcgen::CertificateParams::new(vec!["peer".into()]);
    cert_params.alg = &rcgen::PKCS_ED25519;
    let cert = rcgen::Certificate::from_params(cert_params)?;

    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.serialize_private_key_der()));
    let cert = CertificateDer::from(cert.serialize_der()?);
    Ok((cert, key))
}

/// Setup an endpoint for Quic connections with a given socket address and certificate
pub async fn make_server_endpoint(
    socket: impl AsyncUdpSocket,
    cert_der: CertificateDer<'static>,
    priv_key_der: PrivateKeyDer<'static>,
    known_peers: KnownPeers,
    client_verification: bool,
) -> anyhow::Result<Endpoint> {
    let (server_config, client_config) =
        configure_server(cert_der, priv_key_der, known_peers, client_verification)?;

    let mut endpoint = quinn::Endpoint::new_with_abstract_socket(
        Default::default(),
        Some(server_config),
        Arc::new(socket),
        Arc::new(quinn::TokioRuntime),
    )?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

pub async fn make_server_endpoint_basic_socket(
    socket: tokio::net::UdpSocket,
    cert_der: CertificateDer<'static>,
    priv_key_der: PrivateKeyDer<'static>,
    known_peers: KnownPeers,
) -> anyhow::Result<Endpoint> {
    let (server_config, client_config) =
        configure_server(cert_der, priv_key_der, known_peers, true)?;

    let mut endpoint = quinn::Endpoint::new(
        Default::default(),
        Some(server_config),
        socket.into_std()?,
        Arc::new(quinn::TokioRuntime),
    )?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

/// Given a Quic connection, get the TLS certificate
pub fn get_certificate_from_connection(
    conn: &Connection,
) -> Result<CertificateDer<'static>, UiServerErrorWrapper> {
    let identity = conn
        .peer_identity()
        .ok_or_else(|| UiServerError::ConnectionError("No peer certificate".to_string()))?;

    let remote_cert = identity
        .downcast::<Vec<CertificateDer>>()
        .map_err(|_| UiServerError::ConnectionError("No certificate".to_string()))?;

    Ok(remote_cert
        .first()
        .ok_or_else(|| UiServerError::ConnectionError("No peer certificate".to_string()))?
        .clone())
}

/// Returns default server configuration along with its certificate.
fn configure_server(
    cert_der: CertificateDer<'static>,
    priv_key: PrivateKeyDer<'static>,
    known_peers: KnownPeers,
    client_verification: bool,
) -> anyhow::Result<(ServerConfig, ClientConfig)> {
    let cert_chain = vec![cert_der];

    let crypto = rustls::ServerConfig::builder();

    let crypto = if client_verification {
        crypto.with_client_cert_verifier(ClientVerification::new(known_peers.clone()))
    } else {
        crypto.with_client_cert_verifier(SkipClientVerification::new())
    };

    let mut crypto = crypto.with_single_cert(cert_chain.clone(), priv_key.clone_key())?;

    let supported_protocols: Vec<_> = SUPPORTED_PROTOCOL_VERSIONS
        .into_iter()
        .map(|p| p.to_vec())
        .collect();

    crypto.alpn_protocols = supported_protocols.clone();

    let mut server_config =
        ServerConfig::with_crypto(Arc::<QuicServerConfig>::new(crypto.try_into()?));

    Arc::get_mut(&mut server_config.transport)
        .ok_or_else(|| anyhow!("Cannot get transport config"))?
        .max_concurrent_uni_streams(0_u8.into())
        .keep_alive_interval(Some(KEEP_ALIVE_INTERVAL))
        .max_idle_timeout(Some(IDLE_TIMEOUT.try_into()?));

    let client_crypto_builder = rustls::ClientConfig::builder();

    let client_crypto_builder = rustls::client::danger::DangerousClientConfigBuilder {
        cfg: client_crypto_builder,
    };
    let mut client_crypto = client_crypto_builder
        .with_custom_certificate_verifier(ServerVerification::new(known_peers))
        .with_client_cert_resolver(SimpleClientCertResolver::new(cert_chain, priv_key));

    client_crypto.alpn_protocols = supported_protocols;

    let mut client_config = ClientConfig::new(Arc::new(QuicClientConfig::try_from(client_crypto)?));

    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));
    transport.max_idle_timeout(Some(IDLE_TIMEOUT.try_into()?));
    transport.max_concurrent_uni_streams(0_u8.into());

    client_config.transport_config(Arc::new(transport));

    Ok((server_config, client_config))
}

#[derive(Debug)]
struct ServerVerification {
    known_peers: KnownPeers,
}

impl ServerVerification {
    fn new(known_peers: KnownPeers) -> Arc<Self> {
        Arc::new(Self { known_peers })
    }
}

impl rustls::client::danger::ServerCertVerifier for ServerVerification {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let owned_bytes = end_entity.as_ref().to_vec();
        let owned_cert = CertificateDer::from(owned_bytes);

        // This internally verifies the signature
        let (name, _) = certificate_to_name(owned_cert)?;
        if self.known_peers.has(&name) {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .ok_or_else(|| rustls::Error::General("No crypto provider installed".into()))?;

        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &provider.signature_verification_algorithms,
        )?;

        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .ok_or_else(|| rustls::Error::General("No crypto provider installed".into()))?;

        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &provider.signature_verification_algorithms,
        )?;

        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

#[derive(Debug)]
struct ClientVerification {
    known_peers: KnownPeers,
}

impl ClientVerification {
    fn new(known_peers: KnownPeers) -> Arc<Self> {
        Arc::new(Self { known_peers })
    }
}

impl rustls::server::danger::ClientCertVerifier for ClientVerification {
    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        let owned_bytes = end_entity.as_ref().to_vec();
        let owned_cert = CertificateDer::from(owned_bytes);
        // This internally verifies the signature
        let (name, _) = certificate_to_name(owned_cert)?;
        if self.known_peers.has(&name) {
            Ok(rustls::server::danger::ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ))
        }
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .ok_or_else(|| rustls::Error::General("No crypto provider installed".into()))?;

        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &provider.signature_verification_algorithms,
        )?;

        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .ok_or_else(|| rustls::Error::General("No crypto provider installed".into()))?;

        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &provider.signature_verification_algorithms,
        )?;

        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

#[derive(Debug)]
struct SkipClientVerification;

impl SkipClientVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::server::danger::ClientCertVerifier for SkipClientVerification {
    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        let owned_bytes = end_entity.as_ref().to_vec();
        let owned_cert = CertificateDer::from(owned_bytes);
        // This internally verifies the signature
        let _ = certificate_to_name(owned_cert)?;
        Ok(rustls::server::danger::ClientCertVerified::assertion())
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .ok_or_else(|| rustls::Error::General("No crypto provider installed".into()))?;

        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &provider.signature_verification_algorithms,
        )?;

        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .ok_or_else(|| rustls::Error::General("No crypto provider installed".into()))?;

        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &provider.signature_verification_algorithms,
        )?;

        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

#[derive(Debug)]
struct SimpleClientCertResolver {
    cert_chain: Vec<CertificateDer<'static>>,
    our_signing_key: Arc<OurSigningKey>,
}

impl SimpleClientCertResolver {
    fn new(
        cert_chain: Vec<CertificateDer<'static>>,
        priv_key_der: PrivateKeyDer<'static>,
    ) -> Arc<Self> {
        Arc::new(Self {
            cert_chain,
            our_signing_key: Arc::new(OurSigningKey { priv_key_der }),
        })
    }
}

impl rustls::client::ResolvesClientCert for SimpleClientCertResolver {
    fn has_certs(&self) -> bool {
        true
    }

    fn resolve(
        &self,
        _acceptable_issuers: &[&[u8]],
        _sigschemes: &[rustls::SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(Arc::new(rustls::sign::CertifiedKey::new(
            self.cert_chain.clone(),
            self.our_signing_key.clone(),
        )))
    }
}

#[derive(Debug)]
struct OurSigningKey {
    priv_key_der: PrivateKeyDer<'static>,
}

impl rustls::sign::SigningKey for OurSigningKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn rustls::sign::Signer>> {
        if offered.contains(&SignatureScheme::ED25519) {
            match Ed25519KeyPair::from_pkcs8(self.priv_key_der.secret_der()) {
                Ok(keypair) => Some(Box::new(OurSigner { keypair })),
                Err(_) => {
                    warn!("Cannot create Ed25519KeyPair - bad key given");
                    None
                }
            }
        } else {
            None
        }
    }

    fn algorithm(&self) -> rustls::SignatureAlgorithm {
        rustls::SignatureAlgorithm::ED25519
    }
}

#[derive(Debug)]
struct OurSigner {
    keypair: Ed25519KeyPair,
}

impl rustls::sign::Signer for OurSigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        debug!("Signing client authentication message");
        let sig = self.keypair.sign(message);
        Ok(sig.as_ref().to_vec())
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ED25519
    }
}
