//! Configuration for QUIC connections to remote peers
use anyhow::anyhow;
use log::{debug, warn};
use quinn::{AsyncUdpSocket, ClientConfig, Connection, Endpoint, ServerConfig};
use ring::signature::Ed25519KeyPair;
use rustls::{Certificate, SignatureScheme};
use std::{sync::Arc, time::Duration};

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// Generate a TLS certificate with Ed25519 keypair
pub fn generate_certificate() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    // TODO server name probably shouldn't be localhost but maybe it doesnt matter
    let mut cert_params = rcgen::CertificateParams::new(vec!["localhost".into()]);
    cert_params.alg = &rcgen::PKCS_ED25519;
    let cert = rcgen::Certificate::from_params(cert_params)?;
    let cert_der = cert.serialize_der()?;
    let priv_key = cert.serialize_private_key_der();
    Ok((cert_der, priv_key))
}

/// Setup an endpoint for Quic connections with a given socket address and certificate
pub async fn make_server_endpoint(
    socket: impl AsyncUdpSocket,
    cert_der: Vec<u8>,
    priv_key_der: Vec<u8>,
) -> anyhow::Result<Endpoint> {
    let (server_config, client_config) = configure_server(cert_der, priv_key_der)?;

    let mut endpoint = quinn::Endpoint::new_with_abstract_socket(
        Default::default(),
        Some(server_config),
        socket,
        Arc::new(quinn::TokioRuntime),
    )?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

pub async fn make_server_endpoint_basic_socket(
    socket: tokio::net::UdpSocket,
    cert_der: Vec<u8>,
    priv_key_der: Vec<u8>,
) -> anyhow::Result<Endpoint> {
    let (server_config, client_config) = configure_server(cert_der, priv_key_der)?;

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
pub fn get_certificate_from_connection(conn: &Connection) -> anyhow::Result<Certificate> {
    let identity = conn
        .peer_identity()
        .ok_or_else(|| anyhow!("No peer certificate"))?;

    let remote_cert = identity
        .downcast::<Vec<Certificate>>()
        .map_err(|_| anyhow!("No certificate"))?;
    remote_cert
        .first()
        .ok_or_else(|| anyhow!("No certificate"))
        .cloned()
}

/// Returns default server configuration along with its certificate.
fn configure_server(
    cert_der: Vec<u8>,
    priv_key_der: Vec<u8>,
) -> anyhow::Result<(ServerConfig, ClientConfig)> {
    let priv_key = rustls::PrivateKey(priv_key_der.clone());
    let cert_chain = vec![rustls::Certificate(cert_der)];

    let crypto = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_client_cert_verifier(SkipClientVerification::new())
        .with_single_cert(cert_chain.clone(), priv_key)?;

    let mut server_config = ServerConfig::with_crypto(Arc::new(crypto));

    Arc::get_mut(&mut server_config.transport)
        .ok_or_else(|| anyhow!("Cannot get transport config"))?
        .max_concurrent_uni_streams(0_u8.into())
        .keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));

    let client_crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_client_cert_resolver(SimpleClientCertResolver::new(cert_chain, priv_key_der));

    let client_config = ClientConfig::new(Arc::new(client_crypto));

    Ok((server_config, client_config))
}

struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        debug!("verifying {:?}", _server_name);
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

struct SkipClientVerification;

impl SkipClientVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::server::ClientCertVerifier for SkipClientVerification {
    fn client_auth_root_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _now: std::time::SystemTime,
    ) -> Result<rustls::server::ClientCertVerified, rustls::Error> {
        debug!("verifying client");
        Ok(rustls::server::ClientCertVerified::assertion())
    }
}

struct SimpleClientCertResolver {
    cert_chain: Vec<rustls::Certificate>,
    our_signing_key: Arc<OurSigningKey>,
}

impl SimpleClientCertResolver {
    fn new(cert_chain: Vec<rustls::Certificate>, priv_key_der: Vec<u8>) -> Arc<Self> {
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

struct OurSigningKey {
    priv_key_der: Vec<u8>,
}

impl rustls::sign::SigningKey for OurSigningKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn rustls::sign::Signer>> {
        if offered.contains(&SignatureScheme::ED25519) {
            match Ed25519KeyPair::from_pkcs8(&self.priv_key_der) {
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
