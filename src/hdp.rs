//! Main program loop handling connections to/from peers and messages to/from the UI

use crate::{
    discovery::{topic::Topic, DiscoveredPeer, PeerDiscovery, SessionToken, TOKEN_LENGTH},
    peer::Peer,
    quic::{
        generate_certificate, get_certificate_from_connection, make_server_endpoint,
        make_server_endpoint_basic_socket,
    },
    rpc::Rpc,
    shares::Shares,
    ui_messages::{Command, UiClientMessage, UiEvent, UiResponse, UiServerError, UiServerMessage},
    wire_messages::{Entry, IndexQuery, LsResponse, Request},
    wishlist::{DownloadRequest, WishList},
};
use anyhow::ensure;
use async_stream::try_stream;
use bincode::{deserialize, serialize};
use cryptoxide::{blake2b::Blake2b, digest::Digest};
use futures::{pin_mut, stream::BoxStream, StreamExt};
use harddrive_party_shared::ui_messages::PeerRemoteOrSelf;
use log::{debug, error, info, warn};
use lru::LruCache;
use quinn::{Endpoint, RecvStream};
use rustls::Certificate;
use std::{
    collections::{hash_map, HashMap},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use tokio::{
    fs::create_dir_all,
    select,
    sync::{
        mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
};

const MAX_REQUEST_SIZE: usize = 1024;
const DOWNLOAD_BLOCK_SIZE: usize = 64 * 1024;
const CACHE_SIZE: usize = 256;

/// Key-value store sub-tree names
const CONFIG: &[u8; 1] = b"c";
const TOPIC: &[u8; 1] = b"t";
pub const WISHLIST_BY_PEER: &[u8; 1] = b"p";
pub const WISHLIST_BY_TIMESTAMP: &[u8; 1] = b"T";
pub const DOWNLOADED: &[u8; 1] = b"D";

type IndexCache = LruCache<Request, Vec<Vec<Entry>>>;

pub struct Hdp {
    /// A map of peernames to peer connections
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    /// Remote proceduce call for share queries and downloads
    rpc: Rpc,
    /// The QUIC endpoint
    pub server_connection: ServerConnection,
    /// Channel for commands from the UI
    pub command_tx: UnboundedSender<UiClientMessage>,
    /// Channel for commands from the UI
    command_rx: UnboundedReceiver<UiClientMessage>,
    /// Channel for responses to the UI
    response_tx: UnboundedSender<UiServerMessage>,
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
    pub async fn new(
        storage: impl AsRef<Path>,
        sharedirs: Vec<String>,
        initial_topic_names: Vec<String>,
    ) -> anyhow::Result<(Self, UnboundedReceiver<UiServerMessage>)> {
        // Channels for communication with UI
        let (command_tx, command_rx) = unbounded_channel();
        let (response_tx, response_rx) = unbounded_channel();

        // Local storage db
        let mut db_dir = storage.as_ref().to_owned();
        db_dir.push("db");
        let db = sled::open(db_dir)?;
        let shares = Shares::new(db.clone(), sharedirs).await?;

        let config_db = db.open_tree(CONFIG)?;

        // TODO if in production mode this should be homedir/Downloads
        let mut download_dir = storage.as_ref().to_owned();
        download_dir.push("downloads");
        create_dir_all(&download_dir).await?;

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

        let (name, pk_hash) = certificate_to_name(Certificate(cert_der.clone()));

        // TODO for cross platform support we should use the `home` crate
        let os_home_dir = match std::env::var_os("HOME") {
            Some(o) => o.to_str().map(|s| s.to_string()),
            None => None,
        };

        // Notify the UI of our own details
        send_event(
            response_tx.clone(),
            UiEvent::PeerConnected {
                name: name.clone(),
                peer_type: PeerRemoteOrSelf::Me { os_home_dir },
            },
        );

        let topics = initial_topic_names
            .iter()
            .map(|name| Topic::new(name.to_string()))
            .collect();

        let topics_db = db.open_tree(TOPIC)?;

        // Setup peer discovery
        let (socket_option, peer_discovery) =
            PeerDiscovery::new(topics, true, true, pk_hash, topics_db).await?;

        let server_connection = match socket_option {
            Some(socket) => {
                // Create QUIC endpoint
                ServerConnection::WithEndpoint(
                    make_server_endpoint(socket, cert_der, priv_key_der).await?,
                )
            }
            None => {
                // just give cert_der and priv_key_der
                ServerConnection::Symmetric(cert_der, priv_key_der)
            }
        };

        // Setup db for downloads requests
        let wishlist = WishList::new(&db, response_tx.clone())?;

        Ok((
            Self {
                peers: Default::default(),
                rpc: Rpc::new(shares, response_tx.clone()),
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
        // Inform the UI of initial state of topics and wishlist
        self.topics_updated();
        if let Err(err) = self.wishlist.updated() {
            error!("Error when sending wishlist to UI {err}");
        };

        let (incoming_connection_tx, mut incoming_connection_rx) = unbounded_channel();
        if let ServerConnection::WithEndpoint(endpoint) = self.server_connection.clone() {
            tokio::spawn(async move {
                loop {
                    if let Some(incoming_conn) = endpoint.accept().await {
                        if incoming_connection_tx.send(incoming_conn).is_err() {
                            warn!("Cannot handle incoming connections - channel closed");
                        }
                    }
                }
            });
        }

        loop {
            select! {
                Some(incoming_conn) = incoming_connection_rx.recv() => {
                    self.handle_incoming_connection(incoming_conn).await;
                }
                Some(command) = self.command_rx.recv() => {
                    if let Err(err) = self.handle_command(command).await {
                        error!("Closing connection {err}");
                        break;
                    };
                }
                Some(peer) = self.peer_discovery.peers_rx.recv() => {
                    debug!("Discovered peer {:?}", peer);
                    if self.connect_to_peer(peer).await.is_err() {
                        error!("Cannot connect to discovered peer");
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
        token: Option<SessionToken>,
        remote_cert: Certificate,
    ) {
        let (peer_name, peer_public_key) = certificate_to_name(remote_cert);
        debug!("[{}] Connected to peer {}", self.name, peer_name);
        let response_tx = self.response_tx.clone();

        let peers_clone = self.peers.clone();
        let our_token = self.peer_discovery.session_token;
        let rpc = self.rpc.clone();
        let download_dir = self.download_dir.clone();
        let wishlist = self.wishlist.clone();
        tokio::spawn(async move {
            handle_session_token(&conn, token, our_token).await.unwrap();

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
            );
            loop {
                match accept_incoming_request(&conn).await {
                    Ok((send, buf)) => {
                        rpc.request(buf, send).await;
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
            );
        });
    }

    /// Handle an incoming connection from a remote peer
    async fn handle_incoming_connection(&mut self, incoming_conn: quinn::Connecting) {
        // if self
        //     .peers
        //     .contains_key(&incoming_conn.remote_address().to_string())
        // {
        //     println!("Not conencting to existing peer");
        // } else {
        match incoming_conn.await {
            Ok(conn) => {
                debug!(
                    "incoming QUIC connection accepted {}",
                    conn.remote_address()
                );

                if let Some(i) = conn.handshake_data() {
                    let d = i
                        .downcast::<quinn::crypto::rustls::HandshakeData>()
                        .unwrap();
                    debug!("Server name {:?}", d.server_name);
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
    async fn connect_to_peer(&mut self, peer: DiscoveredPeer) -> Result<UiResponse, UiServerError> {
        let endpoint = match self.server_connection.clone() {
            ServerConnection::WithEndpoint(endpoint) => endpoint,
            ServerConnection::Symmetric(cert_der, priv_key_der) => {
                match peer.socket_option {
                    Some(socket) => {
                        make_server_endpoint_basic_socket(socket, cert_der, priv_key_der)
                            .await
                            .map_err(|_| UiServerError::ConnectionError)?
                    }
                    None => {
                        panic!("no socket")
                    } // TODO
                }
            }
        };

        let connection = endpoint
            .connect(peer.socket_address, "ssss") // TODO
            .map_err(|_| UiServerError::ConnectionError)?
            .await
            .map_err(|_| UiServerError::ConnectionError)?;

        if let Ok(remote_cert) = get_certificate_from_connection(&connection) {
            self.handle_connection(connection, false, Some(peer.token), remote_cert)
                .await;
            Ok(UiResponse::Connect)
        } else {
            Err(UiServerError::ConnectionError)
        }
    }

    /// Handle a command from the UI
    async fn handle_command(
        &mut self,
        ui_client_message: UiClientMessage,
    ) -> Result<(), HandleUiCommandError> {
        let id = ui_client_message.id;
        match ui_client_message.command {
            Command::Join(topic_name) => {
                let topic = Topic::new(topic_name.clone());
                match self.peer_discovery.join_topic(topic).await {
                    Ok(()) => {
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Ok(UiResponse::EndResponse),
                            })
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        }
                        self.topics_updated();
                    }
                    Err(error) => {
                        warn!("Error when joining topic {}", error);
                        // Respond with an error
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Err(UiServerError::JoinOrLeaveError),
                            })
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        }
                    }
                }
            }
            Command::Leave(topic_name) => {
                let topic = Topic::new(topic_name.clone());
                match self.peer_discovery.leave_topic(topic).await {
                    Ok(()) => {
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Ok(UiResponse::EndResponse),
                            })
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        }
                        self.topics_updated();
                    }
                    Err(error) => {
                        warn!("Error when leaving topic {}", error);
                        // Respond with an error
                        if self
                            .response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Err(UiServerError::JoinOrLeaveError),
                            })
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        }
                    }
                }
            }
            Command::Close => {
                // TODO tidy up peer discovery / active transfers
                if let ServerConnection::WithEndpoint(endpoint) = self.server_connection.clone() {
                    endpoint.wait_idle().await;
                }
                // TODO why an error?
                return Err(HandleUiCommandError::ConnectionClosed);
            }
            Command::Connect(_addr) => {
                todo!();
                //     let response = self.connect_to_peer(addr, None).await;
                //     if self
                //         .response_tx
                //         .send(UiServerMessage::Response { id, response })
                //         .is_err()
                //     {
                //         return Err(HandleUiCommandError::ChannelClosed);
                //     }
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
                            tokio::spawn(async move {
                                let ls_response_stream = process_length_prefix(recv).await.unwrap();
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
                    path: Some(path),
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
                            let peer = peers.get(&peer_name).unwrap(); // TODO or send error response
                            peer.public_key
                        };
                        let wishlist = self.wishlist.clone();
                        tokio::spawn(async move {
                            let ls_response_stream = process_length_prefix(recv).await.unwrap();
                            pin_mut!(ls_response_stream);
                            while let Some(Ok(ls_response)) = ls_response_stream.next().await {
                                if let LsResponse::Success(entries) = ls_response {
                                    for entry in entries.iter() {
                                        if !entry.is_dir {
                                            debug!("Adding {} to wishlist", entry.name);

                                            if let Err(err) = wishlist.add(&DownloadRequest::new(
                                                entry.name.clone(),
                                                entry.size,
                                                id,
                                                peer_public_key,
                                            )) {
                                                error!("Cannot make download request {:?}", err);
                                            };
                                        }
                                    }
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
                                .is_err()
                            {
                                error!("Channel closed");
                            }
                            if response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Ok(UiResponse::EndResponse),
                                })
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
                                .is_err()
                            {
                                error!("Channel closed");
                            }
                        }
                    };
                });
            }
        };
        Ok(())
    }

    /// Open a request stream and write a request to the given peer
    async fn request(&self, request: Request, name: &str) -> Result<RecvStream, RequestError> {
        let peers = self.peers.lock().await;
        let peer = peers.get(name).ok_or(RequestError::PeerNotFound)?;
        let (mut send, recv) = peer.connection.open_bi().await?;
        let buf = serialize(&request).map_err(|_| RequestError::SerializationError)?;
        debug!("message serialized, writing...");
        send.write_all(&buf).await?;
        send.finish().await?;
        debug!("message sent");
        Ok(recv)
    }

    /// Called whenever the list of topics changes (user joins or leaves a topic) to inform the UI
    fn topics_updated(&self) {
        if self
            .response_tx
            .send(UiServerMessage::Event(UiEvent::Topics(
                self.peer_discovery.get_topic_names(),
            )))
            .is_err()
        {
            warn!("UI response channel closed");
        }
    }
}

/// Given a TLS certificate, get a 32 byte ID and a human-readable
/// name derived from it.
// TODO the ID should actually just be the public key from the
// certicate, but i cant figure out how to extract it so for now
// just hash the whole thing
fn certificate_to_name(cert: Certificate) -> (String, [u8; 32]) {
    let mut hash = [0u8; 32];
    let mut topic_hash = Blake2b::new(32);
    topic_hash.input(cert.as_ref());
    topic_hash.result(&mut hash);
    (key_to_animal::key_to_name(&hash), hash)
}

fn send_event(sender: UnboundedSender<UiServerMessage>, event: UiEvent) {
    if sender.send(UiServerMessage::Event(event)).is_err() {
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

async fn handle_session_token(
    conn: &quinn::Connection,
    token: Option<SessionToken>,
    our_token: SessionToken,
) -> anyhow::Result<()> {
    if let Some(thier_token) = token {
        let (mut send, _recv) = conn.open_bi().await?;
        send.write_all(&thier_token).await?;
        send.finish().await?;
    } else {
        let (_send, recv) = conn.accept_bi().await?;
        let buf = recv.read_to_end(TOKEN_LENGTH).await?;
        ensure!(buf == our_token, "Rejected remote peer's token");
    }
    Ok(())
}

async fn accept_incoming_request(
    conn: &quinn::Connection,
) -> anyhow::Result<(quinn::SendStream, Vec<u8>)> {
    let (send, recv) = conn.accept_bi().await?;
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

#[cfg(test)]
mod tests {
    use crate::wire_messages::{Entry, ReadQuery};

    use super::*;
    use tempfile::TempDir;

    async fn setup_peer(share_dirs: Vec<String>) -> (Hdp, UnboundedReceiver<UiServerMessage>) {
        let storage = TempDir::new().unwrap();
        Hdp::new(storage, share_dirs, vec!["foo".to_string()])
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_read() -> Result<(), Box<dyn std::error::Error>> {
        env_logger::init();
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

        while let Some(res) = bob_rx.recv().await {
            println!("Res {:?}", res);

            match res {
                UiServerMessage::Event(UiEvent::PeerConnected { name, peer_type: _ }) => {
                    if name == alice_name {
                        println!("connected to {}", name);
                        break;
                    }
                }
                _ => {}
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
                id: 1,
                command: Command::Read(req, alice_name.clone()),
            })
            .unwrap();

        while let Some(res) = bob_rx.recv().await {
            println!("Res {:?}", res);
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
                id: 2,
                command: Command::Ls(req, Some(alice_name)),
            })
            .unwrap();

        let mut entries = Vec::new();
        while let Some(res) = bob_rx.recv().await {
            match res {
                UiServerMessage::Response { id, response } => match response.unwrap() {
                    UiResponse::Ls(LsResponse::Success(some_entries), _name) => {
                        for entry in some_entries {
                            entries.push(entry);
                        }
                    }
                    UiResponse::EndResponse => {
                        if id == 2 {
                            break;
                        }
                    }
                    _ => {}
                },
                _ => {}
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
                // TODO unwrap
                f.write_str(&endpoint.local_addr().unwrap().to_string())?;
            }
            ServerConnection::Symmetric(_, _) => {
                f.write_str("Behind symmetric NAT")?;
            }
        }
        Ok(())
    }
}
