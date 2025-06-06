//! Main program loop handling connections to/from peers and messages to/from the UI

use crate::{
    discovery::{DiscoveredPeer, DiscoveryMethod, PeerDiscovery},
    peer::Peer,
    quic::{
        generate_certificate, get_certificate_from_connection, make_server_endpoint,
        make_server_endpoint_basic_socket,
    },
    rpc::Rpc,
    shares::Shares,
    ui_messages::{Command, UiClientMessage, UiEvent, UiResponse, UiServerError, UiServerMessage},
    wire_messages::{AnnouncePeer, Entry, IndexQuery, LsResponse, Request},
    wishlist::{DownloadRequest, RequestedFile, WishList},
};
use async_stream::try_stream;
use bincode::{deserialize, serialize};
use cryptoxide::{blake2b::Blake2b, digest::Digest};
use futures::{pin_mut, stream::BoxStream, StreamExt};
use harddrive_party_shared::ui_messages::{DownloadInfo, DownloadResponse, PeerRemoteOrSelf};
use log::{debug, error, info, warn};
use lru::LruCache;
use quinn::{Endpoint, RecvStream};
use rustls::Certificate;
use std::{
    collections::{hash_map, HashMap},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use thiserror::Error;
use tokio::{
    select,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Mutex,
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

/// A harddrive-party instance
pub struct Hdp {
    /// A map of peernames to peer connections
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    /// Remote proceduce call for share queries and downloads
    rpc: Rpc,
    /// The QUIC endpoint and TLS certificate
    pub server_connection: ServerConnection,
    /// Channel for commands from the UI (outgoing)
    pub command_tx: Sender<UiClientMessage>,
    /// Channel for commands from the UI (incoming)
    command_rx: Receiver<UiClientMessage>,
    /// Channel for responses to the UI (outgoing)
    response_tx: Sender<UiServerMessage>,
    /// Peer discovery
    peer_discovery: PeerDiscovery,
    /// Download directory
    pub download_dir: PathBuf,
    /// Cache for remote peer's file index
    ls_cache: Arc<Mutex<HashMap<String, IndexCache>>>,
    /// A name derived from our public key
    pub name: String,
    /// Maintains lists of requested/downloaded files
    wishlist: WishList,
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
    ) -> anyhow::Result<(Self, Receiver<UiServerMessage>)> {
        // Channels for communication with UI
        let (command_tx, command_rx) = channel(1024);
        let (response_tx, response_rx) = channel(65536);

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

        // Notify the UI of our own details
        send_event(
            response_tx.clone(),
            UiEvent::PeerConnected {
                name: name.clone(),
                peer_type: PeerRemoteOrSelf::Me {
                    os_home_dir,
                    announce_address: peer_discovery.get_ui_announce_address()?,
                },
            },
        )
        .await;

        // Setup db for downloads/requests
        let wishlist = WishList::new(&db)?;

        Ok((
            Self {
                peers,
                rpc: Rpc::new(
                    shares,
                    response_tx.clone(),
                    peer_discovery.peer_announce_tx.clone(),
                ),
                server_connection,
                command_tx,
                command_rx,
                response_tx,
                peer_discovery,
                download_dir,
                name,
                ls_cache: Default::default(),
                wishlist,
            },
            response_rx,
        ))
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
                // A command from the UI
                Some(command) = self.command_rx.recv() => {
                    if let Err(err) = self.handle_command(command).await {
                        error!("Closing connection {err}");
                        break;
                    };
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
        self.peer_discovery.get_ui_announce_address()
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
        debug!("[{}] Connected to peer {}", self.name, peer_name);
        let response_tx = self.response_tx.clone();

        let discovery_method = if let Some(discovery_method) = discovery_method {
            Some(discovery_method)
        } else {
            self.peer_discovery.get_pending_peer(&conn.remote_address())
        };

        let announce_address = if let Some(discovery_method) = discovery_method {
            // If we have a request id we should send a UI response that the peer has
            // successfully connected
            // Ideally if we encounter a problem we should send an error response
            if let Some(id) = discovery_method.get_request_id() {
                if self
                    .response_tx
                    .send(UiServerMessage::Response {
                        id,
                        response: Ok(UiResponse::EndResponse),
                    })
                    .await
                    .is_err()
                {
                    warn!("Response channel closed");
                }
            }

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

        let peers_clone = self.peers.clone();
        let rpc = self.rpc.clone();
        let download_dir = self.download_dir.clone();
        let wishlist = self.wishlist.clone();

        tokio::spawn(async move {
            {
                // Add peer to our hashmap
                let peer = Peer::new(
                    conn.clone(),
                    response_tx.clone(),
                    download_dir,
                    peer_public_key,
                    wishlist,
                );
                // TODO here we should check our wishlist and make any outstanding requests to this
                // peer
                let mut peers = peers_clone.lock().await;

                if let Some(announce_address) = announce_address {
                    let announce_peer = AnnouncePeer { announce_address };

                    // Send their annouce details to other peers who we are connected to
                    for peer in peers.values() {
                        let request = Request::AnnouncePeer(announce_peer.clone());
                        if let Err(err) = Self::request_peer(request, peer).await {
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
            send_event(
                response_tx.clone(),
                UiEvent::PeerConnected {
                    name: peer_name.clone(),
                    peer_type: PeerRemoteOrSelf::Remote,
                },
            )
            .await;

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

            let mut peers = peers_clone.lock().await;
            if peers.remove(&peer_name).is_none() {
                warn!("Connection closed but peer not present in map");
            }
            debug!("Connection closed - removed peer");
            send_event(
                response_tx,
                UiEvent::PeerDisconnected {
                    name: peer_name.clone(),
                },
            )
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

    /// Handle a command from the UI
    /// This should only return fatal errors - errors relating to handling the command should be
    /// sent to the UI
    async fn handle_command(
        &mut self,
        ui_client_message: UiClientMessage,
    ) -> Result<(), HandleUiCommandError> {
        let id = ui_client_message.id;
        match ui_client_message.command {
            Command::Close => {
                // TODO tidy up peer discovery / active transfers
                if let ServerConnection::WithEndpoint(endpoint) = self.server_connection.clone() {
                    endpoint.wait_idle().await;
                }
                // TODO call flush on sled db
                return Ok(());
            }
            Command::Ls(query, peer_name_option) => {
                // If no name given send the query to all connected peers
                let requests = match peer_name_option {
                    Some(name) => {
                        vec![(Request::Ls(query), name)]
                    }
                    None => {
                        let peers = self.peers.lock().await;
                        peers
                            .keys()
                            .map(|peer_name| (Request::Ls(query.clone()), peer_name.to_string()))
                            .collect()
                    }
                };
                debug!("Making request to {} peers", requests.len());

                // If there is no request to make (no peers), end the response
                if requests.is_empty()
                    && self
                        .response_tx
                        .send(UiServerMessage::Response {
                            id,
                            response: Ok(UiResponse::EndResponse),
                        })
                        .await
                        .is_err()
                {
                    warn!("Response channel closed");
                }

                // Track how many remaining requests there are, so we can terminate the reponse
                // when all are finished
                let remaining_responses: Arc<Mutex<usize>> = Arc::new(Mutex::new(requests.len()));

                for (request, peer_name) in requests {
                    // First check the local cache for an existing response
                    let mut cache = self.ls_cache.lock().await;

                    if let hash_map::Entry::Occupied(mut peer_cache_entry) =
                        cache.entry(peer_name.clone())
                    {
                        let peer_cache = peer_cache_entry.get_mut();
                        if let Some(responses) = peer_cache.get(&request) {
                            debug!("Found existing responses in cache");
                            for entries in responses.iter() {
                                if self
                                    .response_tx
                                    .send(UiServerMessage::Response {
                                        id,
                                        response: Ok(UiResponse::Ls(
                                            LsResponse::Success(entries.to_vec()),
                                            peer_name.to_string(),
                                        )),
                                    })
                                    .await
                                    .is_err()
                                {
                                    warn!("Response channel closed");
                                    break;
                                }
                            }
                            // Terminate with an endresponse
                            // If there was more then one peer we need to only
                            // send this if we are the last one
                            let mut remaining = remaining_responses.lock().await;
                            *remaining -= 1;
                            if *remaining == 0
                                && self
                                    .response_tx
                                    .send(UiServerMessage::Response {
                                        id,
                                        response: Ok(UiResponse::EndResponse),
                                    })
                                    .await
                                    .is_err()
                            {
                                warn!("Response channel closed");
                                break;
                            }
                            continue;
                        }
                    }

                    debug!("Sending ls query to {}", peer_name);
                    let req_clone = request.clone();
                    let peer_name_clone = peer_name.clone();

                    match self.request(request, &peer_name).await {
                        Ok(recv) => {
                            let response_tx = self.response_tx.clone();
                            let remaining_responses_clone = remaining_responses.clone();
                            let ls_cache = self.ls_cache.clone();
                            let ls_response_stream = {
                                match process_length_prefix(recv).await {
                                    Ok(ls_response_stream) => ls_response_stream,
                                    Err(error) => {
                                        warn!("Could not process length prefix {}", error);
                                        return Err(HandleUiCommandError::ConnectionClosed);
                                    }
                                }
                            };
                            tokio::spawn(async move {
                                // TODO handle error
                                pin_mut!(ls_response_stream);

                                let mut cached_entries = Vec::new();
                                while let Some(Ok(ls_response)) = ls_response_stream.next().await {
                                    // If it is not an err, add it to the local
                                    // cache
                                    if let LsResponse::Success(entries) = ls_response.clone() {
                                        cached_entries.push(entries);
                                    }

                                    if response_tx
                                        .send(UiServerMessage::Response {
                                            id,
                                            response: Ok(UiResponse::Ls(
                                                ls_response,
                                                peer_name_clone.to_string(),
                                            )),
                                        })
                                        .await
                                        .is_err()
                                    {
                                        warn!("Response channel closed");
                                        break;
                                    }
                                }
                                if !cached_entries.is_empty() {
                                    debug!("Writing ls cache {}", cached_entries.len());
                                    let mut cache = ls_cache.lock().await;
                                    let peer_cache =
                                        cache.entry(peer_name_clone.clone()).or_insert(
                                            // Unwrap ok here becasue CACHE_SIZE is non-zero
                                            LruCache::new(NonZeroUsize::new(CACHE_SIZE).unwrap()),
                                        );
                                    peer_cache.put(req_clone, cached_entries);
                                }

                                // Terminate with an endresponse
                                // If there was more then one peer we need to only
                                // send this if we are the last one
                                let mut remaining = remaining_responses_clone.lock().await;
                                *remaining -= 1;
                                if *remaining == 0
                                    && response_tx
                                        .send(UiServerMessage::Response {
                                            id,
                                            response: Ok(UiResponse::EndResponse),
                                        })
                                        .await
                                        .is_err()
                                {
                                    warn!("Response channel closed");
                                }
                            });
                        }
                        Err(err) => {
                            error!("Error from remote peer following ls query {:?}", err);
                            // TODO map the error
                            if self
                                .response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Err(UiServerError::RequestError),
                                })
                                .await
                                .is_err()
                            {
                                return Err(HandleUiCommandError::ChannelClosed);
                            }
                        }
                    }
                }
            }
            Command::Shares(query) => {
                // Query our own share index
                // TODO should probably do this in a separate task
                match self
                    .rpc
                    .shares
                    .query(query.path, query.searchterm, query.recursive)
                {
                    Ok(response_iterator) => {
                        for res in response_iterator {
                            if self
                                .response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Ok(UiResponse::Shares(res)),
                                })
                                .await
                                .is_err()
                            {
                                warn!("Response channel closed");
                                break;
                            };
                        }
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Ok(UiResponse::EndResponse),
                            })
                            .await
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        }
                    }
                    Err(error) => {
                        warn!("Error querying own shares {:?}", error);
                        // TODO send this err to UI
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Err(UiServerError::RequestError),
                            })
                            .await
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        };
                    }
                }
            }
            Command::Download { path, peer_name } => {
                // Get details of the file / dir
                let ls_request = Request::Ls(IndexQuery {
                    path: Some(path.clone()),
                    searchterm: None,
                    recursive: true,
                });
                // let mut cache = self.ls_cache.lock().await;
                //
                // if let hash_map::Entry::Occupied(mut peer_cache_entry) =
                //     cache.entry(peer_name.clone())
                // {
                //     let peer_cache = peer_cache_entry.get_mut();
                //     if let Some(responses) = peer_cache.get(&ls_request) {
                //         debug!("Found existing responses in cache");
                //         for entries in responses.iter() {
                //             for entry in entries.iter() {
                //                 debug!("Adding {} to wishlist dir: {}", entry.name, entry.is_dir);
                //             }
                //         }
                //     } else {
                //         debug!("Found nothing in cache");
                //     }
                // }

                match self.request(ls_request, &peer_name).await {
                    Ok(recv) => {
                        let peer_public_key = {
                            let peers = self.peers.lock().await;
                            match peers.get(&peer_name) {
                                Some(peer) => peer.public_key,
                                None => {
                                    warn!("Handling request to download a file from a peer who is not connected");
                                    if self
                                        .response_tx
                                        .send(UiServerMessage::Response {
                                            id,
                                            response: Err(UiServerError::RequestError),
                                        })
                                        .await
                                        .is_err()
                                    {
                                        return Err(HandleUiCommandError::ChannelClosed);
                                    } else {
                                        return Ok(());
                                    }
                                }
                            }
                        };
                        let response_tx = self.response_tx.clone();
                        let wishlist = self.wishlist.clone();
                        tokio::spawn(async move {
                            if let Ok(ls_response_stream) = process_length_prefix(recv).await {
                                pin_mut!(ls_response_stream);
                                while let Some(Ok(ls_response)) = ls_response_stream.next().await {
                                    if let LsResponse::Success(entries) = ls_response {
                                        for entry in entries.iter() {
                                            if entry.name == path {
                                                if let Err(err) =
                                                    wishlist.add_request(&DownloadRequest::new(
                                                        entry.name.clone(),
                                                        entry.size,
                                                        id,
                                                        peer_public_key,
                                                    ))
                                                {
                                                    error!("Cannot add download request {:?}", err);
                                                }
                                            }
                                            if !entry.is_dir {
                                                debug!("Adding {} to wishlist", entry.name);

                                                if let Err(err) =
                                                    wishlist.add_requested_file(&RequestedFile {
                                                        path: entry.name.clone(),
                                                        size: entry.size,
                                                        request_id: id,
                                                        downloaded: false,
                                                    })
                                                {
                                                    error!(
                                                        "Cannot make download request {:?}",
                                                        err
                                                    );
                                                };
                                            }
                                        }
                                    }
                                }
                                // Inform the UI that the request has been made
                                if response_tx
                                    .send(UiServerMessage::Response {
                                        id,
                                        response: Ok(UiResponse::Download(DownloadResponse {
                                            download_info: DownloadInfo::Requested(get_timestamp()),
                                            path,
                                            peer_name,
                                        })),
                                    })
                                    .await
                                    .is_err()
                                {
                                    // log error
                                }
                            }
                        });
                    }
                    Err(error) => {
                        error!("Error from remote peer when making query {:?}", error);
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Err(UiServerError::RequestError),
                            })
                            .await
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        }
                    }
                }
            }
            Command::Read(read_query, peer_name) => {
                let request = Request::Read(read_query);

                match self.request(request, &peer_name).await {
                    Ok(mut recv) => {
                        let response_tx = self.response_tx.clone();
                        tokio::spawn(async move {
                            let mut buf: [u8; DOWNLOAD_BLOCK_SIZE] = [0; DOWNLOAD_BLOCK_SIZE];
                            let mut bytes_read: u64 = 0;
                            // TODO handle errors here
                            while let Ok(Some(n)) = recv.read(&mut buf).await {
                                bytes_read += n as u64;
                                debug!("Read {} bytes", bytes_read);

                                if response_tx
                                    .send(UiServerMessage::Response {
                                        id,
                                        response: Ok(UiResponse::Read(buf[..n].to_vec())),
                                    })
                                    .await
                                    .is_err()
                                {
                                    warn!("Response channel closed");
                                    break;
                                };
                            }
                            // Terminate with an endresponse
                            if response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Ok(UiResponse::EndResponse),
                                })
                                .await
                                .is_err()
                            {
                                warn!("Response channel closed");
                            }
                        });
                    }

                    Err(err) => {
                        error!("Error from remote peer following read request {:?}", err);
                        // TODO map the error
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Err(UiServerError::RequestError),
                            })
                            .await
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        }
                    }
                }
            }
            // Add a directory to share
            Command::AddShare(share_dir) => {
                let response_tx = self.response_tx.clone();
                let mut shares = self.rpc.shares.clone();
                tokio::spawn(async move {
                    match shares.scan(&share_dir).await {
                        Ok(num_added) => {
                            info!("{} shares added", num_added);
                            if response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Ok(UiResponse::AddShare(num_added)),
                                })
                                .await
                                .is_err()
                            {
                                error!("Channel closed");
                            }
                            if response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Ok(UiResponse::EndResponse),
                                })
                                .await
                                .is_err()
                            {
                                error!("Channel closed");
                            }
                        }
                        Err(err) => {
                            warn!("Error adding share dir {}", err);
                            if response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Err(UiServerError::ShareError(err.to_string())),
                                })
                                .await
                                .is_err()
                            {
                                error!("Channel closed");
                            }
                        }
                    };
                });
            }
            Command::RemoveShare(share_name) => {
                let response_tx = self.response_tx.clone();
                let mut shares = self.rpc.shares.clone();
                tokio::spawn(async move {
                    match shares.remove_share_dir(&share_name) {
                        Ok(()) => {
                            info!("{} no longer shared", share_name);
                            if response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Ok(UiResponse::EndResponse),
                                })
                                .await
                                .is_err()
                            {
                                error!("Channel closed");
                            }
                        }
                        Err(err) => {
                            warn!("Error removing share dir {}", err);
                            if response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Err(UiServerError::ShareError(err.to_string())),
                                })
                                .await
                                .is_err()
                            {
                                error!("Channel closed");
                            }
                        }
                    };
                });
            }
            Command::RequestedFiles(request_id) => {
                match self.wishlist.requested_files(request_id) {
                    Ok(response_iterator) => {
                        for res in response_iterator {
                            if self
                                .response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Ok(UiResponse::RequestedFiles(res)),
                                })
                                .await
                                .is_err()
                            {
                                warn!("Response channel closed");
                                break;
                            };
                        }
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Ok(UiResponse::EndResponse),
                            })
                            .await
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        }
                    }
                    Err(error) => {
                        error!("Error getting requested files from wishlist {:?}", error);
                        // TODO more detailed error should be forwarded
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Err(UiServerError::RequestError),
                            })
                            .await
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        };
                    }
                }
            }
            Command::RemoveRequest(_request_id) => {
                // TODO self.wishlist.remove_request
                todo!();
            }
            Command::Requests => {
                match self.wishlist.requested() {
                    Ok(response_iterator) => {
                        for res in response_iterator {
                            if self
                                .response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Ok(UiResponse::Requests(res)),
                                })
                                .await
                                .is_err()
                            {
                                warn!("Response channel closed");
                                break;
                            };
                        }
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Ok(UiResponse::EndResponse),
                            })
                            .await
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        }
                    }
                    Err(error) => {
                        error!("Error getting requests from wishlist {:?}", error);
                        // TODO more detailed error should be forwarded
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Err(UiServerError::RequestError),
                            })
                            .await
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        };
                    }
                }
            }
            Command::ConnectDirect(remote_peer) => {
                if let Err(err) = self
                    .peer_discovery
                    .connect_direct_to_peer(&remote_peer, id)
                    .await
                {
                    if self
                        .response_tx
                        .send(UiServerMessage::Response {
                            id,
                            response: Err(UiServerError::ConnectionError(err.to_string())),
                        })
                        .await
                        .is_err()
                    {
                        return Err(HandleUiCommandError::ChannelClosed);
                    }
                }
                // The EndResponse is sent by the connection handler when a connection is established
            }
        };
        Ok(())
    }

    /// Open a request stream and write a request to the given peer
    async fn request(&self, request: Request, name: &str) -> Result<RecvStream, RequestError> {
        let peers = self.peers.lock().await;
        let peer = peers.get(name).ok_or(RequestError::PeerNotFound)?;
        Self::request_peer(request, peer).await
    }

    async fn request_peer(request: Request, peer: &Peer) -> Result<RecvStream, RequestError> {
        let (mut send, recv) = peer.connection.open_bi().await?;
        let buf = serialize(&request).map_err(|_| RequestError::SerializationError)?;
        debug!("message serialized, writing...");
        send.write_all(&buf).await?;
        send.finish().await?;
        debug!("message sent");
        Ok(recv)
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

async fn send_event(sender: Sender<UiServerMessage>, event: UiEvent) {
    if sender.send(UiServerMessage::Event(event)).await.is_err() {
        warn!("UI response channel closed");
    }
}

/// A stream of Ls responses
type LsResponseStream = BoxStream<'static, anyhow::Result<LsResponse>>;

/// Process responses that are prefixed with their length in bytes
async fn process_length_prefix(mut recv: RecvStream) -> anyhow::Result<LsResponseStream> {
    // Read the length prefix
    // TODO this should be a varint
    let mut length_buf: [u8; 8] = [0; 8];
    let stream = try_stream! {
        while let Ok(()) = recv.read_exact(&mut length_buf).await {
            let length: u64 = u64::from_le_bytes(length_buf);
            debug!("Read prefix {length}");

            // Read a message
            let length_usize: usize = length.try_into()?;
            let mut msg_buf = vec![Default::default(); length_usize];
            match recv.read_exact(&mut msg_buf).await {
                Ok(()) => {
                    let ls_response: LsResponse = deserialize(&msg_buf)?;
                    yield ls_response;
                }
                Err(_) => {
                    warn!("Bad prefix / read error");
                    break;
                }
            }
        }
    };
    Ok(stream.boxed())
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

#[cfg(test)]
mod tests {
    use crate::wire_messages::{Entry, ReadQuery};

    use super::*;
    use tempfile::TempDir;

    async fn setup_peer(share_dirs: Vec<String>) -> (Hdp, Receiver<UiServerMessage>) {
        let storage = TempDir::new().unwrap();
        let downloads = storage.path().to_path_buf();
        Hdp::new(storage, share_dirs, downloads, true)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_read() -> Result<(), Box<dyn std::error::Error>> {
        let (mut alice, _alice_rx) = setup_peer(vec!["tests/test-data".to_string()]).await;
        let alice_name = alice.name.clone();

        let alice_command_tx = alice.command_tx.clone();
        tokio::spawn(async move {
            alice.run().await;
        });

        let (mut bob, mut bob_rx) = setup_peer(vec![]).await;
        let bob_command_tx = bob.command_tx.clone();
        tokio::spawn(async move {
            bob.run().await;
        });

        // Wait until they connect to each other using mDNS
        while let Some(res) = bob_rx.recv().await {
            if let UiServerMessage::Event(UiEvent::PeerConnected { name, peer_type: _ }) = res {
                if name == alice_name {
                    break;
                }
            }
        }

        // Do a read request
        let req = ReadQuery {
            path: "test-data/somefile".to_string(),
            start: None,
            end: None,
        };
        bob_command_tx
            .send(UiClientMessage {
                id: 2,
                command: Command::Read(req, alice_name.clone()),
            })
            .await
            .unwrap();

        while let Some(res) = bob_rx.recv().await {
            if let UiServerMessage::Response { id: _, response } = res {
                assert_eq!(Ok(UiResponse::Read(b"boop\n".to_vec())), response);
                break;
            }
        }

        // // Do an Ls query
        let req = IndexQuery {
            path: None,
            searchterm: None,
            recursive: true,
        };
        bob_command_tx
            .send(UiClientMessage {
                id: 3,
                command: Command::Ls(req, Some(alice_name)),
            })
            .await
            .unwrap();

        let mut entries = Vec::new();
        while let Some(res) = bob_rx.recv().await {
            if let UiServerMessage::Response { id, response } = res {
                match response.unwrap() {
                    UiResponse::Ls(LsResponse::Success(some_entries), _name) => {
                        for entry in some_entries {
                            entries.push(entry);
                        }
                    }
                    UiResponse::EndResponse => {
                        if id == 3 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        let test_entries = create_test_entries();
        assert_eq!(test_entries, entries);

        // Close the connection
        alice_command_tx
            .send(UiClientMessage {
                id: 3,
                command: Command::Close,
            })
            .await
            .unwrap();
        Ok(())
    }

    fn create_test_entries() -> Vec<Entry> {
        vec![
            Entry {
                name: "".to_string(),
                size: 17,
                is_dir: true,
            },
            Entry {
                name: "test-data".to_string(),
                size: 17,
                is_dir: true,
            },
            Entry {
                name: "test-data/subdir".to_string(),
                size: 12,
                is_dir: true,
            },
            Entry {
                name: "test-data/subdir/subsubdir".to_string(),
                size: 6,
                is_dir: true,
            },
            Entry {
                name: "test-data/somefile".to_string(),
                size: 5,
                is_dir: false,
            },
            Entry {
                name: "test-data/subdir/anotherfile".to_string(),
                size: 6,
                is_dir: false,
            },
            Entry {
                name: "test-data/subdir/subsubdir/yetanotherfile".to_string(),
                size: 6,
                is_dir: false,
            },
        ]
    }
}
