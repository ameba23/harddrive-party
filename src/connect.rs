use anyhow::anyhow;
use quinn::{ClientConfig, Endpoint, ServerConfig};
use std::{net::SocketAddr, sync::Arc, time::Duration};

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// Setup an endpoint for Quic connections with a given socket address
pub fn make_server_endpoint(bind_addr: SocketAddr) -> anyhow::Result<(Endpoint, Vec<u8>)> {
    let (server_config, server_cert, client_config) = configure_server()?;
    let mut endpoint = Endpoint::server(server_config, bind_addr)?;
    endpoint.set_default_client_config(client_config);
    Ok((endpoint, server_cert))
}

/// Returns default server configuration along with its certificate.
// #[allow(clippy::field_reassign_with_default)] // https://github.com/rust-lang/rust-clippy/issues/6527
fn configure_server() -> anyhow::Result<(ServerConfig, Vec<u8>, ClientConfig)> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.serialize_der().unwrap();
    let priv_key = cert.serialize_private_key_der();
    let priv_key = rustls::PrivateKey(priv_key);
    let cert_chain = vec![rustls::Certificate(cert_der.clone())];

    // let mut server_config = ServerConfig::with_single_cert(cert_chain, priv_key)?;

    let crypto = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        // .with_client_cert_verifier(SkipClientVerification::new())
        .with_single_cert(cert_chain, priv_key)
        .unwrap();

    let mut server_config = ServerConfig::with_crypto(Arc::new(crypto));

    // let transport_config =
    //     TransportConfig::default().keep_alive_interval(Some(Duration::from_secs(10)));
    Arc::get_mut(&mut server_config.transport)
        .ok_or_else(|| anyhow!("Cannot get transport config"))?
        .max_concurrent_uni_streams(0_u8.into())
        .keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));

    let client_config = configure_client();

    Ok((server_config, cert_der, client_config))
}

/// Setup Quic client for outgoing connections to peers
fn configure_client() -> ClientConfig {
    let crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();
    // .with_client_cert_resolver(SimpleClientCertResolver::new(cert_chain, priv_key));

    ClientConfig::new(Arc::new(crypto))
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
        println!("verifying {:?}", _server_name);
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

// struct SkipClientVerification;
//
// impl SkipClientVerification {
//     fn new() -> Arc<Self> {
//         Arc::new(Self)
//     }
// }
//
// impl rustls::server::ClientCertVerifier for SkipClientVerification {
//     fn client_auth_root_subjects(&self) -> Option<rustls::DistinguishedNames> {
//         Some(Vec::new())
//     }
//
//     fn verify_client_cert(
//         &self,
//         _end_entity: &rustls::Certificate,
//         _intermediates: &[rustls::Certificate],
//         _now: std::time::SystemTime,
//     ) -> Result<rustls::server::ClientCertVerified, rustls::Error> {
//         println!("verifying client");
//         Ok(rustls::server::ClientCertVerified::assertion())
//     }
// }

// struct SimpleClientCertResolver {
//     cert_chain: Vec<rustls::Certificate>,
//     priv_key: PrivateKey,
// }
//
// impl SimpleClientCertResolver {
//     fn new(cert_chain: Vec<rustls::Certificate>, priv_key: PrivateKey) -> Arc<Self> {
//         Arc::new(Self {
//             cert_chain,
//             priv_key,
//         })
//     }
// }
//
// impl rustls::client::ResolvesClientCert for SimpleClientCertResolver {
//     fn has_certs(&self) -> bool {
//         true
//     }
//
//     fn resolve(
//         &self,
//         acceptable_issuers: &[&[u8]],
//         sigschemes: &[rustls::SignatureScheme],
//     ) -> Option<Arc<rustls::sign::CertifiedKey>> {
//         None
//         // Some(Arc::new(rustls::sign::CertifiedKey::new(
//         //     self.cert_chain,
//         //     Arc::new(self.priv_key),
//         // )))
//     }
// }
