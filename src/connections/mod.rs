//! Main program loop handling connections to/from peers
pub mod discovery;
pub mod known_peers;
pub mod quic;
pub mod rpc;
pub mod speedometer;

use crate::{
    connections::{
        discovery::{DiscoveredPeer, DiscoveryMethod, PeerDiscovery},
        quic::{
            generate_certificate, get_certificate_from_connection, make_server_endpoint,
            make_server_endpoint_basic_socket,
        },
        rpc::Rpc,
    },
    errors::UiServerErrorWrapper,
    peer::Peer,
    subtree_names::{CONFIG, KNOWN_PEERS_V1},
    ui_messages::{PeerInfo, UiEvent, UiServerError},
    wire_messages::{AnnouncePeer, Request},
    SharedState,
};
use harddrive_party_shared::{
    wire_messages::{AnnounceAddress, PeerConnectionDetails},
    PeerId,
};
use log::{debug, error, info, warn};
use quinn::Endpoint;
use rand::Rng;
use ring::signature::{Ed25519KeyPair, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    net::UdpSocket,
    select,
    sync::{mpsc, Mutex},
    time::timeout,
};
use x509_parser::prelude::{FromDer, X509Certificate};

/// The maximum number of bytes a request message may be
const MAX_REQUEST_SIZE: usize = 1024;

/// Number of times to attempt to reconnect to a peer following lost connection
const RECONNECT_MAX_ATTEMPTS: usize = 20;
/// Initial delay for reconnection attempt (backs off exponentially)
const RECONNECT_INITIAL_DELAY: Duration = Duration::from_secs(1);
/// Maximum backoff delay
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(120);
/// How long to wait for the QUIC endpoint to drain during graceful shutdown.
const SHUTDOWN_WAIT_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
/// A harddrive-party instance
pub struct Hdp {
    /// Shared state between UI server and backend
    pub shared_state: SharedState,
    /// Remote proceduce call for share queries and downloads
    rpc: Rpc,
    /// The QUIC endpoint and TLS certificate
    pub server_connection: ServerConnection,
    /// Peer discovery
    peer_discovery: PeerDiscovery,
    /// Channel for graceful shutdown signal
    graceful_shutdown_rx: mpsc::Receiver<()>,
}

impl Hdp {
    /// Constructor which also returns a [Receiver] for UI events
    /// Takes:
    /// - The directory to store the database
    /// - Initial directories to share, if any
    /// - The path to store downloaded files
    /// - Whether to use mDNS to discover peers on the local network
    /// - Optional local address for QUIC peer connections
    /// - Optional custom STUN servers
    pub async fn new(
        storage: impl AsRef<Path>,
        share_dirs: Vec<String>,
        download_dir: PathBuf,
        use_mdns: bool,
        local_addr: Option<SocketAddr>,
        stun_servers: Option<Vec<String>>,
    ) -> anyhow::Result<Self> {
        // Local storage db
        let mut db_dir = storage.as_ref().to_owned();
        db_dir.push("db");
        let db = sled::open(db_dir)?;
        let config_db = db.open_tree(CONFIG)?;

        // Attempt to get keypair / certificate from storage, and otherwise generate them and store
        let (cert_der, priv_key_der) = {
            let existing_cert = config_db.get(b"cert");
            let existing_priv = config_db.get(b"priv");
            match (existing_cert, existing_priv) {
                (Ok(Some(cert_der)), Ok(Some(priv_key_der))) => (
                    CertificateDer::from(cert_der.to_vec()),
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(priv_key_der.to_vec())),
                ),
                _ => {
                    let (cert_der, priv_key_der) = generate_certificate()?;
                    config_db.insert(b"cert", cert_der.as_ref())?;
                    config_db.insert(b"priv", priv_key_der.secret_der())?;
                    (cert_der, priv_key_der)
                }
            }
        };

        // Derive a human-readable name from the public key
        let (name, peer_id) =
            certificate_to_name(CertificateDer::from_slice(&cert_der.clone()).into_owned())?;

        let peers: Arc<Mutex<HashMap<PeerId, Peer>>> = Default::default();

        // Read the port from storage
        // We attempt to use the same port as last time if possible, so that if the process is
        // stopped and restarted, peers can reconnect without needing to exchange details again
        let last_used_port = local_addr
            .is_none()
            .then(|| {
                config_db
                    .get(b"port")
                    .ok()
                    .flatten()
                    .and_then(|bytes| bytes.to_vec().try_into().ok())
                    .map(u16::from_be_bytes)
            })
            .flatten();

        let known_peers_db = db.open_tree(KNOWN_PEERS_V1)?;

        // Setup peer discovery
        let (socket_option, peer_discovery) = PeerDiscovery::new(
            use_mdns,
            peer_id,
            peers.clone(),
            local_addr,
            last_used_port,
            known_peers_db,
            stun_servers,
        )
        .await?;

        let (graceful_shutdown_tx, graceful_shutdown_rx) = mpsc::channel(1);
        let signing_key = Ed25519KeyPair::from_pkcs8(priv_key_der.secret_der())
            .map_err(|_| anyhow::anyhow!("stored Ed25519 private key is invalid"))?;
        if signing_key.public_key().as_ref() != peer_id.as_bytes() {
            return Err(anyhow::anyhow!(
                "stored certificate and Ed25519 private key do not match"
            ));
        }
        let signing_bytes = AnnouncePeer::signing_bytes(&peer_discovery.announce_address)?;
        let self_announcement = AnnouncePeer {
            announce_address: peer_discovery.announce_address.clone(),
            signature: signing_key
                .sign(&signing_bytes)
                .as_ref()
                .try_into()
                .map_err(|_| anyhow::anyhow!("Ed25519 produced an invalid signature length"))?,
        };

        // Setup shared state used by UI server
        let shared_state = SharedState::new(
            db,
            share_dirs,
            download_dir,
            peer_id,
            name,
            peer_discovery.peer_announce_tx.clone(),
            peers,
            peer_discovery.announce_address.clone(),
            self_announcement,
            graceful_shutdown_tx,
            peer_discovery.known_peers.clone(),
        )
        .await?;

        let server_connection = match socket_option {
            Some(socket) => {
                // Get the port we are bound to:
                if let Ok(port) = socket.get_port() {
                    // Write port to config
                    let port_bytes = port.to_be_bytes();
                    config_db.insert(b"port", &port_bytes)?;
                }

                // Create QUIC endpoint
                ServerConnection::WithEndpoint(
                    make_server_endpoint(
                        socket,
                        cert_der,
                        priv_key_der,
                        shared_state.known_peers.clone(),
                        peer_discovery.use_client_verification(),
                    )
                    .await?,
                )
            }
            None => {
                // This is for the case that we are behind an unfriendly NAT and don't have a fixed
                // socket that peers can connect to
                // Give cert_der and priv_key_der which we use to create an endpoint on a different
                // port for each connecting peer
                ServerConnection::Symmetric(cert_der, priv_key_der)
            }
        };

        Ok(Self {
            shared_state: shared_state.clone(),
            rpc: Rpc::new(
                shared_state.shares,
                shared_state.event_broadcaster,
                peer_discovery.peer_announce_tx.clone(),
                shared_state.peers.clone(),
                shared_state.known_peers.clone(),
                shared_state.verified_announcements.clone(),
            ),
            server_connection,
            peer_discovery,
            graceful_shutdown_rx,
        })
    }

    /// Loop handling incoming peer connections, and discovered peers
    pub async fn run(&mut self) {
        let (incoming_connection_tx, mut incoming_connection_rx) = mpsc::channel(1024);
        if let ServerConnection::WithEndpoint(endpoint) = self.server_connection.clone() {
            tokio::spawn(async move {
                loop {
                    match endpoint.accept().await {
                        Some(incoming_conn) => {
                            if incoming_connection_tx.send(incoming_conn).await.is_err() {
                                warn!("Cannot handle incoming connections - channel closed");
                                break;
                            }
                        }
                        None => {
                            debug!("Endpoint closed; stopping incoming connection task");
                            break;
                        }
                    }
                }
            });
        }

        // Look at our known peers, and if there are any without NAT, connect to them
        for announce_address in self.shared_state.known_peers.iter() {
            if let PeerConnectionDetails::NoNat(socket_address) =
                announce_address.connection_details
            {
                let peer = DiscoveredPeer {
                    socket_address,
                    socket_option: None,
                    discovery_method: DiscoveryMethod::Direct,
                    announce_address,
                };
                info!(
                    "Connecting to known peer... {}",
                    PeerInfo::from_id(peer.announce_address.public_key).name
                );
                spawn_outbound_connect(
                    self.server_connection.clone(),
                    self.shared_state.clone(),
                    self.rpc.clone(),
                    peer,
                );
            }
        }

        loop {
            select! {
                // An incoming peer connection
                Some(incoming_conn) = incoming_connection_rx.recv() => {
                    let maybe_peer_details = self.peer_discovery.get_pending_peer(&incoming_conn.remote_address());

                    if let Err(err) = handle_incoming_connection(
                        self.shared_state.clone(),
                        self.rpc.clone(),
                        maybe_peer_details.clone(),
                        incoming_conn,
                    ).await {
                        error!("Error when handling incoming peer connection {err:?}");
                         if let Some((_, announce_address)) = maybe_peer_details {
                            let peer = PeerInfo::from_id(announce_address.public_key);
                             self.shared_state.send_event(UiEvent::PeerConnectionFailed { peer, error: err.to_string() }).await;
                        }
                    }
                }
                // A discovered peer
                Some(peer) = self.peer_discovery.peers_rx.recv() => {
                    debug!("Discovered peer {peer:?}");
                    spawn_outbound_connect(
                        self.server_connection.clone(),
                        self.shared_state.clone(),
                        self.rpc.clone(),
                        peer,
                    );
                }
                // A signal for graceful shutdown
                Some(()) = self.graceful_shutdown_rx.recv() => {
                    debug!("Shutting down");
                    let connections = {
                        let peers = self.shared_state.peers.lock().await;
                        peers
                            .values()
                            .map(|peer| peer.connection.clone())
                            .collect::<Vec<_>>()
                    };
                    for connection in connections {
                        connection.close(0u32.into(), b"shutdown");
                    }
                    if let ServerConnection::WithEndpoint(endpoint) = self.server_connection.clone() {
                        endpoint.close(0u32.into(), b"shutdown");
                        if timeout(SHUTDOWN_WAIT_IDLE_TIMEOUT, endpoint.wait_idle())
                            .await
                            .is_err()
                        {
                            warn!(
                                "Timed out waiting for QUIC endpoint to go idle during shutdown"
                            );
                        }
                    }
                    return;
                }
            }
        }
    }
}

fn spawn_outbound_connect(
    server_connection: ServerConnection,
    shared_state: SharedState,
    rpc: Rpc,
    peer: DiscoveredPeer,
) {
    let peer_id = peer.announce_address.public_key;
    let peer_info = PeerInfo::from_id(peer_id);
    tokio::spawn(async move {
        if !begin_outbound_connect(&shared_state, peer_id).await {
            debug!(
                "Skipping outbound connect to {} ({}); already connected or pending",
                peer_info.name, peer_id
            );
            return;
        }

        let result = connect_to_peer(server_connection, shared_state.clone(), rpc, peer).await;
        shared_state
            .pending_outbound_connections
            .lock()
            .await
            .remove(&peer_id);

        if let Err(err) = result {
            error!(
                "Cannot connect to peer {} ({}): {err:?}",
                peer_info.name, peer_id
            );
            shared_state
                .send_event(UiEvent::PeerConnectionFailed {
                    peer: peer_info,
                    error: err.to_string(),
                })
                .await;
        }
    });
}

/// Reserve an outbound connect slot unless the peer is already connected or pending.
async fn begin_outbound_connect(shared_state: &SharedState, peer_id: PeerId) -> bool {
    if shared_state
        .pending_outbound_connections
        .lock()
        .await
        .contains(&peer_id)
    {
        return false;
    }

    if shared_state.peers.lock().await.contains_key(&peer_id) {
        return false;
    }

    let mut pending = shared_state.pending_outbound_connections.lock().await;
    if pending.contains(&peer_id) {
        return false;
    }

    pending.insert(peer_id);
    true
}

/// Handle a QUIC connection from/to another peer.
async fn handle_connection(
    shared_state: SharedState,
    rpc: Rpc,
    conn: quinn::Connection,
    incoming: bool,
    maybe_peer_details: Option<(DiscoveryMethod, AnnounceAddress)>,
    remote_cert: CertificateDer<'static>,
) -> Result<(), UiServerError> {
    let (peer_name, peer_id) = certificate_to_name(remote_cert)
        .map_err(|err| UiServerError::PeerDiscovery(err.to_string()))?;
    if peer_id == shared_state.id {
        conn.close(1u32.into(), b"self connection");
        return Err(UiServerError::ConnectionError(
            "Refusing connection to our own peer identity".to_string(),
        ));
    }
    let peer_info = PeerInfo {
        id: peer_id,
        name: peer_name.clone(),
    };
    if maybe_peer_details
        .as_ref()
        .is_some_and(|(_, address)| address.public_key != peer_id)
    {
        conn.close(1u32.into(), b"announce key does not match TLS certificate");
        return Err(UiServerError::ConnectionError(
            "Announced public key does not match TLS certificate".to_string(),
        ));
    }

    debug!("[{}] Connected to peer {}", shared_state.name, peer_name);

    let announce_address = maybe_peer_details.map(|peer_details| peer_details.1);
    let conn_stable_id = conn.stable_id();

    tokio::spawn(async move {
        let peer_connection = {
            let manually_disconnected = shared_state
                .manually_disconnected_peers
                .lock()
                .await
                .contains(&peer_id);
            if manually_disconnected {
                debug!("Rejecting connection from {peer_name} because it is manually disconnected");
                conn.close(0u32.into(), b"manually disconnected");
                return;
            }

            let mut peers = shared_state.peers.lock().await;
            if let Some(existing_peer) = peers.get(&peer_id) {
                if existing_peer.connection.close_reason().is_none() {
                    warn!("Duplicate connection for {peer_name}; keeping existing connection");
                    conn.close(0u32.into(), b"duplicate connection");
                    return;
                }

                let replaced_peer = peers.remove(&peer_id).expect("peer should exist after get");
                debug!("Replacing existing connection for {peer_name}");
                replaced_peer
                    .connection
                    .close(0u32.into(), b"replaced connection");
            }

            let peer = Peer::new(
                conn.clone(),
                shared_state.event_broadcaster.clone(),
                shared_state.download_dir.clone(),
                peer_id,
                shared_state.wishlist.clone(),
                None,
            );
            let peer_connection = peer.connection.clone();
            peers.insert(peer_id, peer);
            let direction = if incoming { "incoming" } else { "outgoing" };
            info!("[{}] connected to {} peers", direction, peers.len());
            peer_connection
        };

        shared_state
            .manually_disconnected_peers
            .lock()
            .await
            .remove(&peer_id);

        let self_announce = Request::AnnouncePeer(shared_state.self_announcement.clone());
        if let Err(err) = SharedState::request_connection(self_announce, &peer_connection).await {
            warn!("Could not self-announce to {peer_name}: {err}");
        }

        // Existing records are already signature-verified. Send them only after our
        // self-announcement so the receiver can authenticate this connection first.
        let verified_announcements = shared_state
            .verified_announcements
            .lock()
            .await
            .values()
            .filter(|announcement| announcement.peer_id() != peer_id)
            .cloned()
            .collect::<Vec<_>>();
        for announcement in verified_announcements {
            let request = Request::AnnouncePeer(announcement);
            if let Err(err) = SharedState::request_connection(request, &peer_connection).await {
                error!("Failed to send announce message to new peer: {err:?}");
            }
        }
        shared_state
            .send_event(UiEvent::PeerConnected {
                peer: peer_info.clone(),
            })
            .await;

        let err = loop {
            match accept_incoming_request(&conn).await {
                Ok((send, buf)) => {
                    rpc.request(buf, send, peer_id).await;
                }
                Err(err) => {
                    warn!("Failed to handle request: {err:?}");
                    break err;
                }
            }
        };

        let (was_connected, reconnect_announce_address) = {
            let mut peers = shared_state.peers.lock().await;
            let is_current_connection = peers
                .get(&peer_id)
                .is_some_and(|peer| peer.connection.stable_id() == conn_stable_id);
            let maybe_announce_address = peers.get(&peer_id).and_then(|peer| {
                peer.announcement
                    .as_ref()
                    .map(|announcement| announcement.announce_address.clone())
            });
            if is_current_connection {
                peers.remove(&peer_id);
            }
            (
                is_current_connection,
                announce_address.clone().or(maybe_announce_address),
            )
        };

        if was_connected {
            info!("Peer disconnected: {} ({})", peer_name, err);
            shared_state
                .send_event(UiEvent::PeerDisconnected {
                    peer: peer_info.clone(),
                    error: err.to_string(),
                })
                .await;
        } else {
            debug!(
                "Suppressing disconnect event for {} because a replacement connection is active",
                peer_name
            );
        }

        if shared_state
            .shutting_down
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            debug!(
                "Skipping reconnect to {} because shutdown is in progress",
                peer_name
            );
        } else if shared_state
            .manually_disconnected_peers
            .lock()
            .await
            .contains(&peer_id)
        {
            debug!(
                "Skipping reconnect to {} because it was intentionally disconnected",
                peer_name
            );
        } else if was_connected {
            if let Some(announce_address) = reconnect_announce_address {
                if let Err(err) = shared_state.connect_to_peer(announce_address).await {
                    warn!("Could not reconnect to peer following disconnect: {err}");
                }
            }
        } else {
            debug!(
                "Skipping reconnect to {} because a replacement connection is already active",
                peer_name
            );
        }
    });
    Ok(())
}

/// Handle an incoming connection from a remote peer.
async fn handle_incoming_connection(
    shared_state: SharedState,
    rpc: Rpc,
    maybe_peer_details: Option<(DiscoveryMethod, AnnounceAddress)>,
    incoming_conn: quinn::Incoming,
) -> Result<(), UiServerErrorWrapper> {
    let conn = incoming_conn.await?;
    debug!(
        "Incoming QUIC connection accepted {}",
        conn.remote_address()
    );

    if let Some(i) = conn.handshake_data() {
        if let Ok(handshake_data) = i.downcast::<quinn::crypto::rustls::HandshakeData>() {
            debug!(
                "Server name of connecting peer {:?}",
                handshake_data.server_name
            );
        }
    }

    let c = conn.clone();
    let remote_cert = get_certificate_from_connection(&c)?;
    handle_connection(
        shared_state,
        rpc,
        conn,
        true,
        maybe_peer_details,
        remote_cert,
    )
    .await?;
    Ok(())
}

/// Initiate a Quic connection to a remote peer
async fn connect_to_peer(
    server_connection: ServerConnection,
    shared_state: SharedState,
    rpc: Rpc,
    peer: DiscoveredPeer,
) -> Result<(), UiServerError> {
    let DiscoveredPeer {
        socket_address,
        socket_option,
        discovery_method,
        announce_address,
    } = peer;

    let endpoint = match server_connection {
        ServerConnection::WithEndpoint(endpoint) => endpoint,
        ServerConnection::Symmetric(cert_der, priv_key_der) => {
            let socket = match socket_option {
                Some(socket) => socket,
                None => UdpSocket::bind("0.0.0.0:0")
                    .await
                    .map_err(|e| UiServerError::PeerDiscovery(e.to_string()))?,
            };

            make_server_endpoint_basic_socket(
                socket,
                cert_der,
                priv_key_der,
                shared_state.known_peers.clone(),
            )
            .await
            .map_err(|err| {
                UiServerError::ConnectionError(format!("When creating endpoint: {err:?}"))
            })?
        }
    };

    let connection =
        connect_with_backoff(&endpoint, socket_address, &announce_address, "peer").await?;

    let remote_cert = get_certificate_from_connection(&connection).map_err(|err| {
        UiServerError::ConnectionError(format!("When getting certificate: {err:?}"))
    })?;

    handle_connection(
        shared_state,
        rpc,
        connection,
        false,
        Some((discovery_method, announce_address)),
        remote_cert,
    )
    .await?;

    Ok(())
}

/// Given a TLS certificate, get a 32 byte public key and a human-readable
/// name derived from it.
/// This internally verifies the signature, and only accepts Ed25519.
pub fn certificate_to_name(
    cert: CertificateDer<'static>,
) -> Result<(String, PeerId), rustls::Error> {
    let (_, cert) = X509Certificate::from_der(&cert)
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;

    cert.verify_signature(None)
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadSignature))?;

    let public_key = cert.public_key();
    // We only accept Ed25519
    if public_key.algorithm.algorithm.to_string() != "1.3.101.112" {
        return Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::BadEncoding,
        ));
    }
    let public_key: [u8; 32] = public_key
        .subject_public_key
        .data
        .as_ref()
        .try_into()
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;

    Ok((
        key_to_animal::key_to_name(&public_key),
        PeerId::new(public_key),
    ))
}

/// Accept an incoming request on a QUIC connection, and read the request message
async fn accept_incoming_request(
    conn: &quinn::Connection,
) -> anyhow::Result<(quinn::SendStream, Vec<u8>)> {
    let (send, mut recv) = conn.accept_bi().await?;
    let buf = recv.read_to_end(MAX_REQUEST_SIZE).await?;
    Ok((send, buf))
}

/// The QUIC server
/// In the case that our NAT type makes it not possible to have a single UDP endpoint for all peer
/// connections, this stores the certificate details
#[derive(Debug)]
pub enum ServerConnection {
    /// A single endpoint
    WithEndpoint(Endpoint),
    /// Certificate details used to create an endpoint for each peer connection
    Symmetric(CertificateDer<'static>, PrivateKeyDer<'static>),
}

impl std::fmt::Display for ServerConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerConnection::WithEndpoint(endpoint) => {
                write!(
                    f,
                    "{}",
                    match endpoint.local_addr() {
                        Ok(local_addr) => local_addr.to_string(),
                        _ => "No local adddress".to_string(),
                    }
                )?;
            }
            ServerConnection::Symmetric(_, _) => {
                f.write_str("Behind symmetric NAT")?;
            }
        }
        Ok(())
    }
}

impl Clone for ServerConnection {
    fn clone(&self) -> Self {
        match self {
            ServerConnection::WithEndpoint(endpoint) => {
                ServerConnection::WithEndpoint(endpoint.clone())
            }
            ServerConnection::Symmetric(cert, key) => {
                ServerConnection::Symmetric(cert.clone(), key.clone_key())
            }
        }
    }
}

/// Get the current time as a [Duration]
pub fn get_timestamp() -> Duration {
    let system_time = SystemTime::now();
    system_time
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("Time went backwards")
}

/// Adds jitter to reconnection (to avoid all peers connecting at the same time)
fn jittered(delay: Duration) -> Duration {
    let mut rng = rand::thread_rng();

    let base_ms = delay.as_millis() as i64;
    let jitter_ms = (base_ms as f64 * 0.2) as i64;

    let offset = rng.gen_range(-jitter_ms..=jitter_ms);
    let final_ms = (base_ms + offset).max(0) as u64;

    Duration::from_millis(final_ms)
}

/// Connect to a peer, attempting several times with a backoff delay
async fn connect_with_backoff(
    endpoint: &quinn::Endpoint,
    socket_address: SocketAddr,
    announce_address: &AnnounceAddress,
    server_name: &str,
) -> Result<quinn::Connection, UiServerError> {
    let mut delay = RECONNECT_INITIAL_DELAY;

    for attempt in 1..=RECONNECT_MAX_ATTEMPTS {
        let connecting = endpoint
            .connect(socket_address, server_name)
            .map_err(|err| UiServerError::ConnectionError(format!("When connecting: {err:?}")))?;

        match connecting.await {
            Ok(conn) => return Ok(conn),
            Err(err) => {
                // If this was the last attempt, return the error
                if attempt >= RECONNECT_MAX_ATTEMPTS {
                    return Err(UiServerError::ConnectionError(format!(
                        "After connecting to {announce_address} (attempt {attempt}/{RECONNECT_MAX_ATTEMPTS}): {err:?}"
                    )));
                }

                warn!(
                    "Connection to {0} failed: {err} Retrying...",
                    announce_address
                );
                let sleep_for = jittered(delay);
                tokio::time::sleep(sleep_for).await;

                // Exponential backoff with cap
                delay = std::cmp::min(delay.saturating_mul(2), RECONNECT_MAX_DELAY);
            }
        }
    }
    unreachable!("Loop either returns success or error");
}
