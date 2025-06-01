//! Main program loop handling connections to/from peers and messages to/from the UI

use crate::{
    api::UiServerErrorWrapper,
    discovery::{DiscoveredPeer, DiscoveryMethod, PeerConnect, PeerDiscovery},
    peer::Peer,
    quic::{
        generate_certificate, get_certificate_from_connection, make_server_endpoint,
        make_server_endpoint_basic_socket,
    },
    rpc::Rpc,
    shares::Shares,
    ui_messages::{UiEvent, UiServerError},
    wire_messages::{AnnounceAddress, AnnouncePeer, Entry, Request},
    wishlist::WishList,
};
use base64::{prelude::BASE64_STANDARD_NO_PAD, Engine};
use bincode::serialize;
use cryptoxide::{blake2b::Blake2b, digest::Digest};
use harddrive_party_shared::ui_messages::PeerRemoteOrSelf;
use log::{debug, error, info, warn};
use lru::LruCache;
use quinn::{Endpoint, RecvStream};
use rustls::Certificate;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use thiserror::Error;
use tokio::{
    select,
    sync::{
        broadcast,
        mpsc::{channel, Sender},
        oneshot, Mutex,
    },
};

/// The maximum number of bytes a request message may be
const MAX_REQUEST_SIZE: usize = 1024;
/// The maximum bytes transfered at a time when downloading
const DOWNLOAD_BLOCK_SIZE: usize = 64 * 1024;
/// The number of records which will be cached when doing index (`Ls`) queries to a remote peer
/// This saves making subsequent requests with a duplicate query
const CACHE_SIZE: usize = 256;
/// The size in bytes of a public key (certificate hash)
const PUBLIC_KEY_LENGTH: usize = 32;

/// Key-value store sub-tree names
const CONFIG: &[u8; 1] = b"c";
pub const REQUESTS: &[u8; 1] = b"r";
pub const REQUESTS_BY_TIMESTAMP: &[u8; 1] = b"R";
pub const REQUESTS_PROGRESS: &[u8; 1] = b"P";
pub const REQUESTED_FILES_BY_PEER: &[u8; 1] = b"p";
pub const REQUESTED_FILES_BY_REQUEST_ID: &[u8; 1] = b"C";

type PublicKey = [u8; PUBLIC_KEY_LENGTH];
/// The cache for index requests
type IndexCache = LruCache<Request, Vec<Vec<Entry>>>;

#[derive(Clone)]
pub struct SharedState {
    /// A map of peernames to peer connections
    pub peers: Arc<Mutex<HashMap<String, Peer>>>,
    /// The index of shared files
    pub shares: Shares,
    /// Cache for remote peer's file index
    pub ls_cache: Arc<Mutex<HashMap<String, IndexCache>>>,
    /// Maintains lists of requested/downloaded files
    pub wishlist: WishList,
    /// Channel for sending events to the UI
    pub event_broadcaster: broadcast::Sender<UiEvent>,
    /// Channel for announcing peers to connect to
    peer_announce_tx: Sender<PeerConnect>,
    /// Download directory
    pub download_dir: PathBuf,
    /// A name derived from our public key
    pub name: String,
    /// Our own connection details
    pub announce_address: AnnounceAddress,
}

impl SharedState {
    /// Send an event to the UI
    pub async fn send_event(&self, event: UiEvent) {
        if self.event_broadcaster.send(event).is_err() {
            warn!("UI response channel closed");
        }
    }

    /// Open a request stream and write a request to the given peer
    pub async fn request(&self, request: Request, name: &str) -> Result<RecvStream, RequestError> {
        let peers = self.peers.lock().await;
        let peer = peers.get(name).ok_or(RequestError::PeerNotFound)?;
        Self::request_peer(request, peer).await
    }

    pub async fn request_peer(request: Request, peer: &Peer) -> Result<RecvStream, RequestError> {
        let (mut send, recv) = peer.connection.open_bi().await?;
        let buf = serialize(&request).map_err(|_| RequestError::SerializationError)?;
        debug!("message serialized, writing...");
        send.write_all(&buf).await?;
        send.finish().await?;
        debug!("message sent");
        Ok(recv)
    }

    pub fn get_ui_announce_address(&self) -> anyhow::Result<String> {
        let bytes = self.announce_address.to_bytes();
        Ok(BASE64_STANDARD_NO_PAD.encode(&bytes))
    }

    pub async fn connect_to_peer(
        &self,
        announce_address: AnnounceAddress,
    ) -> Result<(), UiServerErrorWrapper> {
        let discovery_method = DiscoveryMethod::Direct {
            announce_address: announce_address.clone(),
        };

        let (response_tx, response_rx) = oneshot::channel();
        let peer_connect = PeerConnect {
            discovery_method,
            response_tx: Some(response_tx),
        };
        self.peer_announce_tx
            .send(peer_connect)
            .await
            .map_err(|_| {
                UiServerError::PeerDiscovery("Peer announce channel closed".to_string())
            })?;

        response_rx.await?
    }
}

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
        let shares = Shares::new(db.clone(), share_dirs).await?;

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

        // Set home dir - this is used in the UI as a placeholder when choosing a directory to
        // share
        // TODO for cross platform support we should use the `home` crate
        let os_home_dir = match std::env::var_os("HOME") {
            Some(o) => o.to_str().map(|s| s.to_string()),
            None => None,
        };

        let peers: Arc<Mutex<HashMap<String, Peer>>> = Default::default();

        // Setup peer discovery
        let (socket_option, peer_discovery) =
            PeerDiscovery::new(use_mdns, pk_hash, peers.clone()).await?;

        let server_connection = match socket_option {
            Some(socket) => {
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

        // Setup db for downloads/requests
        let wishlist = WishList::new(&db)?;

        let (event_broadcaster, _rx) = broadcast::channel(65536);

        let shared_state = SharedState {
            wishlist,
            shares: shares.clone(),
            peers,
            ls_cache: Default::default(),
            event_broadcaster: event_broadcaster.clone(),
            peer_announce_tx: peer_discovery.peer_announce_tx.clone(),
            download_dir,
            name: name.clone(),
            announce_address: peer_discovery.announce_address.clone(),
        };

        // Notify the UI of our own details
        event_broadcaster.send(UiEvent::PeerConnected {
            name,
            peer_type: PeerRemoteOrSelf::Me {
                os_home_dir,
                announce_address: shared_state.get_ui_announce_address()?,
            },
        })?;

        Ok(Self {
            shared_state: shared_state.clone(),
            rpc: Rpc::new(
                shares,
                event_broadcaster,
                peer_discovery.peer_announce_tx.clone(),
            ),
            server_connection,
            peer_discovery,
        })
    }

    /// Loop handling incoming peer connections, commands from the UI, and discovered peers
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
                    self.handle_incoming_connection(incoming_conn).await;
                }
                // A discovered peer
                Some(peer) = self.peer_discovery.peers_rx.recv() => {
                    debug!("Discovered peer {:?}", peer);
                    if let Err(err) = self.connect_to_peer(peer).await {
                        error!("Cannot connect to discovered peer {:?}", err);
                    };
                }
            }
        }
    }

    pub fn get_announce_address(&self) -> anyhow::Result<String> {
        self.shared_state.get_ui_announce_address()
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

        let discovery_method = if let Some(discovery_method) = discovery_method {
            Some(discovery_method)
        } else {
            self.peer_discovery.get_pending_peer(&conn.remote_address())
        };

        let announce_address = if let Some(discovery_method) = discovery_method {
            // If we have a request id we should send a UI response that the peer has
            // successfully connected
            // Ideally if we encounter a problem we should send an error response
            // TODO this should be a oneshot
            // if self
            //     .response_tx
            //     .send(UiServerMessage::Response {
            //         id,
            //         response: Ok(UiResponse::EndResponse),
            //     })
            //     .await
            //     .is_err()
            // {
            //     warn!("Response channel closed");
            // }

            // If we know their announce address, check if it matches the certificate
            if let Some(announce_address) = discovery_method.get_announce_address() {
                if announce_address.public_key == peer_public_key {
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
                );
                // TODO here we should check our wishlist and make any outstanding requests to this
                // peer
                let mut peers = shared_state.peers.lock().await;

                if let Some(announce_address) = announce_address {
                    let announce_peer = AnnouncePeer { announce_address };

                    // Send their annouce details to other peers who we are connected to
                    for peer in peers.values() {
                        let request = Request::AnnouncePeer(announce_peer.clone());
                        if let Err(err) = SharedState::request_peer(request, peer).await {
                            error!("Failed to send announce message to {peer:?} - {err:?}");
                        }
                    }
                }

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
                    peer_type: PeerRemoteOrSelf::Remote,
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
    async fn handle_incoming_connection(&mut self, incoming_conn: quinn::Connecting) {
        match incoming_conn.await {
            Ok(conn) => {
                debug!(
                    "incoming QUIC connection accepted {}",
                    conn.remote_address()
                );

                if let Some(i) = conn.handshake_data() {
                    if let Ok(handshake_data) = i.downcast::<quinn::crypto::rustls::HandshakeData>()
                    {
                        debug!("Server name {:?}", handshake_data.server_name);
                    }
                }

                if let Ok(remote_cert) = get_certificate_from_connection(&conn) {
                    self.handle_connection(conn, true, None, remote_cert).await;
                } else {
                    warn!("Peer attempted to connect with bad or missing certificate");
                }
            }
            Err(err) => {
                warn!("Incoming QUIC connection failed {:?}", err);
            }
        }
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

/// Error on making a request to a given remote peer
#[derive(Error, Debug, PartialEq)]
pub enum RequestError {
    #[error("Peer not found")]
    PeerNotFound,
    #[error(transparent)]
    ConnectionError(#[from] quinn::ConnectionError),
    #[error("Cannot serialize message")]
    SerializationError,
    #[error(transparent)]
    WriteError(#[from] quinn::WriteError),
}

/// Error on handling a UI command
#[derive(Error, Debug)]
pub enum HandleUiCommandError {
    #[error("User closed connection")]
    ConnectionClosed,
    #[error("Channel closed - could not send response")]
    ChannelClosed,
    #[error("Db error")]
    DbError,
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
