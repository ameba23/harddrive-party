//! Main program loop handling connections to/from peers
pub mod discovery;
pub mod quic;
pub mod rpc;

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
    subtree_names::CONFIG,
    ui_messages::{UiEvent, UiServerError},
    wire_messages::{AnnouncePeer, Request},
    SharedState,
};
use cryptoxide::{blake2b::Blake2b, digest::Digest};
use log::{debug, error, info, warn};
use quinn::Endpoint;
use rustls::Certificate;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    select,
    sync::{mpsc::channel, Mutex},
};

/// The maximum number of bytes a request message may be
const MAX_REQUEST_SIZE: usize = 1024;

/// The size in bytes of a public key (certificate hash)
const PUBLIC_KEY_LENGTH: usize = 32;

type PublicKey = [u8; PUBLIC_KEY_LENGTH];

/// A harddrive-party instance
pub struct Hdp {
    pub shared_state: SharedState,
    /// Remote proceduce call for share queries and downloads
    rpc: Rpc,
    /// The QUIC endpoint and TLS certificate
    pub server_connection: ServerConnection,
    /// Peer discovery
    peer_discovery: PeerDiscovery,
}

impl Hdp {
    /// Constructor which also returns a [Receiver] for UI events
    /// Takes:
    /// - The directory to store the database
    /// - Initial directories to share, if any
    /// - The path to store downloaded files
    /// - Whether to use mDNS to discover peers on the local network
    pub async fn new(
        storage: impl AsRef<Path>,
        share_dirs: Vec<String>,
        download_dir: PathBuf,
        use_mdns: bool,
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
                (Ok(Some(cert_der)), Ok(Some(priv_key_der))) => {
                    (cert_der.to_vec(), priv_key_der.to_vec())
                }
                _ => {
                    let (cert_der, priv_key_der) = generate_certificate()?;
                    config_db.insert(b"cert", cert_der.clone())?;
                    config_db.insert(b"priv", priv_key_der.clone())?;
                    (cert_der, priv_key_der)
                }
            }
        };

        // Derive a human-readable name from the public key
        let (name, pk_hash) = certificate_to_name(Certificate(cert_der.clone()));

        let peers: Arc<Mutex<HashMap<String, Peer>>> = Default::default();

        // Read the port from storage
        // We attempt to use the same port as last time if possible, so that if the process is
        // stopped and restarted, peers can reconnect without needing to exchange details again
        let port = config_db
            .get(b"port")
            .ok()
            .flatten()
            .and_then(|bytes| bytes.to_vec().try_into().ok())
            .map(u16::from_be_bytes);

        // Setup peer discovery
        let (socket_option, peer_discovery) =
            PeerDiscovery::new(use_mdns, pk_hash, peers.clone(), port).await?;

        // Setup shared state used by UI server
        let shared_state = SharedState::new(
            db,
            share_dirs,
            download_dir,
            name,
            peer_discovery.peer_announce_tx.clone(),
            peers,
            peer_discovery.announce_address.clone(),
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
                    make_server_endpoint(socket, cert_der, priv_key_der).await?,
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
            ),
            server_connection,
            peer_discovery,
        })
    }

    /// Loop handling incoming peer connections, and discovered peers
    pub async fn run(&mut self) {
        let (incoming_connection_tx, mut incoming_connection_rx) = channel(1024);
        if let ServerConnection::WithEndpoint(endpoint) = self.server_connection.clone() {
            tokio::spawn(async move {
                loop {
                    if let Some(incoming_conn) = endpoint.accept().await {
                        if incoming_connection_tx.send(incoming_conn).await.is_err() {
                            warn!("Cannot handle incoming connections - channel closed");
                        }
                    }
                }
            });
        }

        loop {
            select! {
                // An incoming peer connection
                Some(incoming_conn) = incoming_connection_rx.recv() => {
                    let discovery_method = self.peer_discovery.get_pending_peer(&incoming_conn.remote_address());
                    let announce_address = discovery_method.as_ref().and_then(|d| d.get_announce_address());

                    if let Err(err) = self.handle_incoming_connection(discovery_method, incoming_conn).await {
                        error!("Error when handling incoming peer connection {:?}", err);
                         if let Some (announce_address) = announce_address {
                            let name = announce_address.name;
                             self.shared_state.send_event(UiEvent::PeerConnectionFailed { name, error: err.to_string() }).await;
                        }
                    }
                }
                // A discovered peer
                Some(peer) = self.peer_discovery.peers_rx.recv() => {
                    debug!("Discovered peer {:?}", peer);
                    let name = peer.discovery_method.get_announce_address().map(|a| a.name);

                    if let Err(err) = self.connect_to_peer(peer).await {
                        error!("Cannot connect to discovered peer {:?}", err);
                        if let Some(name) = name {
                            self.shared_state.send_event(UiEvent::PeerConnectionFailed { name, error: err.to_string() }).await;
                        }
                    };
                }
            }
        }
    }

    /// Handle a QUIC connection from/to another peer
    async fn handle_connection(
        &mut self,
        conn: quinn::Connection,
        incoming: bool,
        discovery_method: Option<DiscoveryMethod>,
        remote_cert: Certificate,
    ) {
        let (peer_name, peer_public_key) = certificate_to_name(remote_cert);
        debug!(
            "[{}] Connected to peer {}",
            self.shared_state.name, peer_name
        );

        let announce_address = if let Some(discovery_method) = discovery_method {
            // If we know their announce address, check if it matches the certificate
            if let Some(announce_address) = discovery_method.get_announce_address() {
                if announce_address.name == key_to_animal::key_to_name(&peer_public_key) {
                    debug!("Public key matches");
                } else {
                    // TODO there should be some consequences here - eg: return
                    error!("Public key does not match!");
                }
                Some(announce_address)
            } else {
                None
            }
        } else {
            None
        };

        let rpc = self.rpc.clone();
        let shared_state = self.shared_state.clone();

        tokio::spawn(async move {
            {
                // Add peer to our hashmap
                let peer = Peer::new(
                    conn.clone(),
                    shared_state.event_broadcaster.clone(),
                    shared_state.download_dir.clone(),
                    peer_public_key,
                    shared_state.wishlist.clone(),
                    announce_address.clone(),
                );
                let mut peers = shared_state.peers.lock().await;

                if let Some(announce_address) = announce_address {
                    let announce_peer = AnnouncePeer { announce_address };

                    // Send their annouce details to other peers who we are connected to
                    for other_peer in peers.values() {
                        let request = Request::AnnouncePeer(announce_peer.clone());
                        if let Err(err) = SharedState::request_peer(request, other_peer).await {
                            error!("Failed to send announce message to {other_peer:?} - {err:?}");
                        }

                        // We must also send the announce details of these other peers to this peer
                        if let Some(ref announce_address_other) = peer.announce_address {
                            let announce_other_peer = AnnouncePeer {
                                announce_address: announce_address_other.clone(),
                            };
                            let request = Request::AnnouncePeer(announce_other_peer);
                            if let Err(err) = SharedState::request_peer(request, &peer).await {
                                error!("Failed to send announce message to {peer:?} - {err:?}");
                            }
                        }
                    }
                }

                // TODO here we should check our wishlist and make any outstanding requests to this
                // peer

                if let Some(_existing_peer) = peers.insert(peer_name.clone(), peer) {
                    warn!("Adding connection for already connected peer!");
                };
                let direction = if incoming { "incoming" } else { "outgoing" };
                info!("[{}] connected to {} peers", direction, peers.len());
            }
            // Inform the UI that a new peer has connected
            shared_state
                .send_event(UiEvent::PeerConnected {
                    name: peer_name.clone(),
                })
                .await;

            // Loop over requests from the peer and handle them
            loop {
                match accept_incoming_request(&conn).await {
                    Ok((send, buf)) => {
                        rpc.request(buf, send, peer_name.clone()).await;
                    }
                    Err(err) => {
                        warn!("Failed to handle request: {:?}", err);
                        break;
                    }
                }
            }

            // Remove the peer from our peers map and inform the UI
            let mut peers = shared_state.peers.lock().await;
            if peers.remove(&peer_name).is_none() {
                warn!("Connection closed but peer not present in map");
            }
            debug!("Connection closed - removed peer");
            shared_state
                .send_event(UiEvent::PeerDisconnected {
                    name: peer_name.clone(),
                })
                .await;
        });
    }

    /// Handle an incoming connection from a remote peer
    async fn handle_incoming_connection(
        &mut self,
        discovery_method: Option<DiscoveryMethod>,
        incoming_conn: quinn::Connecting,
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

        let remote_cert = get_certificate_from_connection(&conn)?;
        self.handle_connection(conn, true, discovery_method, remote_cert)
            .await;
        Ok(())
    }

    /// Initiate a Quic connection to a remote peer
    async fn connect_to_peer(&mut self, peer: DiscoveredPeer) -> Result<(), UiServerError> {
        let endpoint = match self.server_connection.clone() {
            ServerConnection::WithEndpoint(endpoint) => endpoint,
            ServerConnection::Symmetric(cert_der, priv_key_der) => {
                match peer.socket_option {
                    Some(socket) => {
                        make_server_endpoint_basic_socket(socket, cert_der, priv_key_der)
                            .await
                            .map_err(|err| {
                                UiServerError::ConnectionError(format!(
                                    "When creating endpoint: {err:?}"
                                ))
                            })?
                    }
                    None => {
                        // This should be an impossible state to get into
                        panic!("We are beind symmetric NAT but didn't get a socket for a connecting peer");
                    }
                }
            }
        };

        let connection = endpoint
            .connect(peer.socket_address, "localhost") // TODO
            .map_err(|err| UiServerError::ConnectionError(format!("When connecting: {err:?}")))?
            .await
            .map_err(|err| UiServerError::ConnectionError(format!("After connecting: {err:?}")))?;

        let remote_cert = get_certificate_from_connection(&connection).map_err(|err| {
            UiServerError::ConnectionError(format!("When getting certificate: {err:?}"))
        })?;
        self.handle_connection(connection, false, Some(peer.discovery_method), remote_cert)
            .await;
        Ok(())
    }
}

/// Given a TLS certificate, get a 32 byte ID and a human-readable
/// name derived from it.
// TODO the ID should actually just be the public key from the
// certicate, but i cant figure out how to extract it so for now
// just hash the whole thing
fn certificate_to_name(cert: Certificate) -> (String, PublicKey) {
    let mut hash = [0u8; 32];
    let mut hasher = Blake2b::new(32);
    hasher.input(cert.as_ref());
    hasher.result(&mut hash);
    (key_to_animal::key_to_name(&hash), hash)
}

async fn accept_incoming_request(
    conn: &quinn::Connection,
) -> anyhow::Result<(quinn::SendStream, Vec<u8>)> {
    let (send, mut recv) = conn.accept_bi().await?;
    let buf = recv.read_to_end(MAX_REQUEST_SIZE).await?;
    Ok((send, buf))
}

#[derive(Clone)]
pub enum ServerConnection {
    /// A single endpoint
    WithEndpoint(Endpoint),
    /// Certificate details used to create an endpoint for each peer connection
    Symmetric(Vec<u8>, Vec<u8>),
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

/// Get the current time as a [Duration]
pub fn get_timestamp() -> Duration {
    let system_time = SystemTime::now();
    system_time
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("Time went backwards")
}
