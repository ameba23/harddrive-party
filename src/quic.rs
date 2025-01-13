//! Configuration for QUIC connections to remote peers
use anyhow::anyhow;
use log::{debug, warn};
use quinn::{
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
    AsyncUdpSocket, ClientConfig, Endpoint, ServerConfig,
};
use ring::signature::Ed25519KeyPair;
use rustls::{pki_types::CertificateDer, SignatureScheme};
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
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    let (server_config, client_config) = configure_server(cert_der, priv_key_der)?;

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
pub fn get_certificate_from_connection<'a>(
    identity: Option<Box<dyn std::any::Any>>, // conn: &'a Connection,
) -> anyhow::Result<CertificateDer<'a>> {
    // let identity = conn
    //     .peer_identity()
    let identity = identity.ok_or_else(|| anyhow!("No peer certificate"))?;

    let remote_cert = identity
        .downcast::<Vec<CertificateDer>>()
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
    let pkd_2 = priv_key_der.clone();
    let priv_key: rustls_pki_types::PrivateKeyDer = priv_key_der.try_into().unwrap();
    // let pkd: &'static [u8] = &priv_key_der;
    // let priv_key = rustls_pki_types::PrivateKeyDer::Pkcs8(pkd.into());
    let cert: rustls_pki_types::CertificateDer = cert_der.into();
    let cert_chain = vec![cert];

    let crypto = rustls::ServerConfig::builder()
        .with_client_cert_verifier(SkipClientVerification::new())
        .with_single_cert(cert_chain.clone(), priv_key)?;

    let conf = QuicServerConfig::try_from(crypto).unwrap();
    let mut server_config = ServerConfig::with_crypto(Arc::new(conf));

    Arc::get_mut(&mut server_config.transport)
        .ok_or_else(|| anyhow!("Cannot get transport config"))?
        .max_concurrent_uni_streams(0_u8.into())
        .keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));

    let client_crypto = rustls::ClientConfig::builder();

    let client_config = rustls::client::danger::DangerousClientConfigBuilder { cfg: client_crypto }
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_client_cert_resolver(SimpleClientCertResolver::new(cert_chain, pkd_2));

    let quic_client_config: QuicClientConfig = Arc::new(client_config).try_into().unwrap();
    let client_config = ClientConfig::new(Arc::new(quic_client_config));

    Ok((server_config, client_config))
}

#[derive(Debug)]
struct SkipServerVerification;

// impl SkipServerVerification {
//     fn new() -> Arc<Self> {
//         Arc::new(Self)
//     }
// }

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        debug!("verifying {:?}", _server_name);
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
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
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        debug!("verifying client");
        Ok(rustls::server::danger::ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

#[derive(Debug)]
struct SimpleClientCertResolver {
    cert_chain: Vec<rustls_pki_types::CertificateDer<'static>>,
    our_signing_key: Arc<OurSigningKey>,
}

impl SimpleClientCertResolver {
    fn new(
        cert_chain: Vec<rustls_pki_types::CertificateDer<'static>>,
        priv_key_der: Vec<u8>,
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
