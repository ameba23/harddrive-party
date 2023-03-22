//! Main program loop handling connections to/from peers and messages to/from the UI

use crate::{
    discovery::{discover_peers, topic::Topic, DiscoveredPeer, SessionToken, TOKEN_LENGTH},
    quic::{generate_certificate, get_certificate_from_connection, make_server_endpoint},
    rpc::Rpc,
    shares::Shares,
    ui_messages::{
        Command, ReadResponse, UiClientMessage, UiEvent, UiResponse, UiServerError, UiServerMessage,
    },
    wire_messages::{Entry, LsResponse, Request},
};
use anyhow::anyhow;
use async_stream::try_stream;
use bincode::{deserialize, serialize};
use cryptoxide::{blake2b::Blake2b, digest::Digest};
use futures::{pin_mut, stream::BoxStream, StreamExt};
use log::{debug, info, warn};
use lru::LruCache;
use quinn::{Connection, Endpoint, RecvStream};
use rustls::Certificate;
use speedometer::Speedometer;
use std::{
    collections::{hash_map, HashMap},
    net::SocketAddr,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::{AsyncSeekExt, AsyncWriteExt},
    select,
    sync::{
        mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
};

const MAX_REQUEST_SIZE: usize = 1024;
const DOWNLOAD_BLOCK_SIZE: usize = 64 * 1024;
const CONFIG: &[u8; 1] = b"c";
const CACHE_SIZE: usize = 256;

type IndexCache = LruCache<Request, Vec<Vec<Entry>>>;

pub struct Hdp {
    /// A map of peernames to peer connections
    peers: Arc<Mutex<HashMap<String, Connection>>>,
    /// Remote proceduce call for share queries and downloads
    rpc: Arc<Rpc>,
    /// The QUIC endpoint
    pub endpoint: Endpoint,
    /// Channel for commands from the UI
    pub command_tx: UnboundedSender<UiClientMessage>,
    /// Channel for commands from the UI
    command_rx: UnboundedReceiver<UiClientMessage>,
    /// Channel for responses to the UI
    response_tx: UnboundedSender<UiServerMessage>,
    /// Channel for discovered peers
    peers_rx: UnboundedReceiver<DiscoveredPeer>,
    /// Session token
    token: SessionToken,
    /// Download directory
    download_dir: PathBuf,
    /// Cache for remote peer's file index
    ls_cache: Arc<Mutex<HashMap<String, IndexCache>>>,
    /// A name derived from our public key
    pub name: String,
}

impl Hdp {
    pub async fn new(
        storage: impl AsRef<Path>,
        sharedirs: Vec<&str>,
        topic_names: Vec<&str>,
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

        let name = certificate_to_name(Certificate(cert_der.clone()));
        // Notify the UI of our own name
        send_event(
            response_tx.clone(),
            UiEvent::PeerConnected {
                name: name.clone(),
                is_self: true,
            },
        );

        let topics = topic_names
            .iter()
            .map(|name| Topic::new(name.to_string()))
            .collect();

        // Setup peer discovery
        let (socket, peers_rx, token) = discover_peers(topics, true, true).await?;

        // Create QUIC endpoint
        let endpoint = make_server_endpoint(socket, cert_der, priv_key_der).await?;

        Ok((
            Self {
                peers: Default::default(),
                rpc: Arc::new(Rpc::new(shares, response_tx.clone())),
                endpoint,
                command_tx,
                command_rx,
                response_tx,
                peers_rx,
                token,
                download_dir,
                // public_key,
                name,
                ls_cache: Default::default(),
            },
            response_rx,
        ))
    }

    /// Loop handling incoming peer connections, commands from the UI, and discovered peers
    pub async fn run(&mut self) {
        loop {
            select! {
                Some(incoming_conn) = self.endpoint.accept() => {
                    self.handle_incoming_connection(incoming_conn).await;
                }
                Some(command) = self.command_rx.recv() => {
                    if let Err(err) = self.handle_command(command).await {
                        warn!("Closing connection {err}");
                        break;
                    };
                }
                Some(peer) = self.peers_rx.recv() => {
                    debug!("Discovered peer {}", peer.addr);
                    if self.connect_to_peer(peer.addr, Some(peer.token)).await.is_err() {
                        warn!("Cannot connect to discovered peer");
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
        let peer_name = certificate_to_name(remote_cert);
        debug!("Connected to peer {}", peer_name);
        let response_tx = self.response_tx.clone();

        let peers_clone = self.peers.clone();
        let our_token = self.token;
        let rpc = self.rpc.clone();
        tokio::spawn(async move {
            if let Some(thier_token) = token {
                let (mut send, _recv) = conn.open_bi().await.unwrap();
                send.write_all(&thier_token).await.unwrap();
                // send.write_all(&our_token).await.unwrap();
                send.finish().await.unwrap();
            } else if let Ok((_send, recv)) = conn.accept_bi().await {
                match recv.read_to_end(TOKEN_LENGTH).await {
                    Ok(buf) => {
                        // make some check
                        if buf == our_token {
                            debug!("accepted remote peer's token");
                        } else {
                            warn!("Rejected remote peer's token");
                            return;
                        }
                    }
                    Err(_) => {
                        return;
                    }
                }
            } else {
                return;
            }

            {
                // Add peer to our hashmap
                let direction = if incoming { "incoming" } else { "outgoing" };
                let mut peers = peers_clone.lock().await;
                if let Some(_existing_connection) = peers.insert(peer_name.clone(), conn.clone()) {
                    warn!("Adding connection for already connected peer!");
                };
                info!("[{}] connected to {} peers", direction, peers.len());
                send_event(
                    response_tx.clone(),
                    UiEvent::PeerConnected {
                        name: peer_name.clone(),
                        is_self: false,
                    },
                );
            }

            // Loop over incoming requests from this peer
            loop {
                match conn.accept_bi().await {
                    Ok((send, recv)) => {
                        match recv.read_to_end(MAX_REQUEST_SIZE).await {
                            Ok(buf) => {
                                let request: Result<Request, Box<bincode::ErrorKind>> =
                                    deserialize(&buf);
                                match request {
                                    Ok(req) => {
                                        debug!("Got request from peer {:?}", req);
                                        match req {
                                            Request::Ls {
                                                path,
                                                searchterm,
                                                recursive,
                                            } => {
                                                if let Ok(()) =
                                                    rpc.ls(path, searchterm, recursive, send).await
                                                {
                                                };
                                            }
                                            Request::Read { path, start, end } => {
                                                if let Ok(()) =
                                                    rpc.read(path, start, end, send).await
                                                {
                                                };
                                            }
                                        }
                                    }
                                    Err(_) => {
                                        warn!("Cannot decode wire message");
                                    }
                                }
                            }
                            Err(err) => {
                                warn!("Cannot read from incoming QUIC stream {:?}", err);
                                break;
                            }
                        };
                    }
                    Err(error) => {
                        match error {
                            quinn::ConnectionError::TimedOut => {
                                warn!("Timeout when accepting stream from peer - likely they have disconnected");
                            }
                            _ => {
                                warn!("Error when accepting QUIC stream {:?}", error);
                            }
                        }
                        break;
                    }
                }
            }
            let mut peers = peers_clone.lock().await;
            match peers.remove(&peer_name) {
                Some(_) => {
                    debug!("Connection closed - removed peer");
                    send_event(
                        response_tx,
                        UiEvent::PeerDisconnected {
                            name: peer_name.clone(),
                        },
                    );
                }
                None => {
                    warn!("Connection closed but peer not present in map");
                }
            }
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
    async fn connect_to_peer(
        &mut self,
        addr: SocketAddr,
        token: Option<SessionToken>,
    ) -> Result<UiResponse, UiServerError> {
        let connection = self
            .endpoint
            .connect(addr, "ssss") // TODO
            .map_err(|_| UiServerError::ConnectionError)?
            .await
            .map_err(|_| UiServerError::ConnectionError)?;

        if let Ok(remote_cert) = get_certificate_from_connection(&connection) {
            self.handle_connection(connection, false, token, remote_cert)
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
            Command::Close => {
                // TODO tidy up peer discovery / active transfers
                self.endpoint.wait_idle().await;
                // TODO why an error?
                return Err(HandleUiCommandError::ConnectionClosed);
            }
            Command::Connect(addr) => {
                let response = self.connect_to_peer(addr, None).await;
                if self
                    .response_tx
                    .send(UiServerMessage::Response { id, response })
                    .is_err()
                {
                    return Err(HandleUiCommandError::ChannelClosed);
                }
            }
            Command::Request(req, name) => {
                // Some querys are to be sent to multiple peers
                let requests = self.expand_request(req, name).await;

                debug!("making request to {} peers", requests.len());
                // If there is no request to make, end the response
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
                    // If it is an ls request, first check the local cache for an existing response
                    if let Request::Ls { .. } = request {
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
                    }

                    info!("Sending request to {}", peer_name);
                    let req_clone = request.clone();
                    let peer_name_clone = peer_name.clone();
                    let download_dir = self.download_dir.clone();
                    match self.request(request, &peer_name).await {
                        Ok(mut recv) => {
                            let response_tx = self.response_tx.clone();
                            let remaining_responses_clone = remaining_responses.clone();
                            let ls_cache = self.ls_cache.clone();
                            tokio::spawn(async move {
                                match req_clone {
                                    Request::Ls {
                                        path: _,
                                        searchterm: _,
                                        recursive: _,
                                    } => {
                                        let ls_response_stream =
                                            process_length_prefix(recv).await.unwrap();
                                        pin_mut!(ls_response_stream);

                                        let mut cached_entries = Vec::new();
                                        while let Some(Ok(ls_response)) =
                                            ls_response_stream.next().await
                                        {
                                            // If it is not an err, add it to the local
                                            // cache
                                            if let LsResponse::Success(entries) =
                                                ls_response.clone()
                                            {
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
                                            let peer_cache = cache
                                                .entry(peer_name_clone.clone())
                                                .or_insert(LruCache::new(
                                                    NonZeroUsize::new(CACHE_SIZE).unwrap(),
                                                ));
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
                                    }
                                    Request::Read {
                                        path,
                                        start,
                                        end: _,
                                    } => {
                                        let output_path = download_dir.join(path.clone());
                                        match setup_download(output_path, start).await {
                                            Ok(mut file) => {
                                                let mut buf: [u8; DOWNLOAD_BLOCK_SIZE] =
                                                    [0; DOWNLOAD_BLOCK_SIZE];
                                                let mut bytes_read: u64 = 0;
                                                let mut total_bytes_read = 0;
                                                let mut speedometer =
                                                    Speedometer::new(Duration::from_secs(5));
                                                // TODO handle errors here
                                                while let Ok(Some(n)) = recv.read(&mut buf).await {
                                                    debug!("Read {} bytes", n);
                                                    speedometer.entry(n);
                                                    bytes_read += n as u64;
                                                    total_bytes_read += n as u64;

                                                    if let Err(error) = file.write(&buf[..n]).await
                                                    {
                                                        warn!(
                                                            "Cannot write downloading file {:?}",
                                                            error
                                                        );
                                                        break;
                                                    }
                                                    if response_tx
                                                        .send(UiServerMessage::Response {
                                                            id,
                                                            response: Ok(UiResponse::Read(
                                                                ReadResponse {
                                                                    path: path.clone(),
                                                                    bytes_read,
                                                                    total_bytes_read,
                                                                    speed: speedometer
                                                                        .measure()
                                                                        .unwrap(),
                                                                },
                                                                // buf[..n].to_vec(),
                                                            )),
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
                                            }
                                            Err(error) => {
                                                warn!(
                                                    "Cannot setup output file for download {:?}",
                                                    error
                                                );
                                            }
                                        };
                                    }
                                }
                            });
                        }
                        Err(err) => {
                            println!("Error from remote peer {:?}", err);
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
                    };
                }
            }
            Command::Shares {
                path,
                searchterm,
                recursive,
            } => {
                match self.rpc.shares.query(path, searchterm, recursive) {
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
                            warn!("Response channel closed");
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
                            warn!("Response channel closed");
                        };
                    }
                }
            }
            Command::Download { path, peer_name } => {
                // Check if entry is a file or dir
                let ls_request = Request::Ls {
                    path: Some(path),
                    searchterm: None,
                    recursive: true,
                };
                debug!("download");
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
                        let ls_response_stream = process_length_prefix(recv).await.unwrap();
                        pin_mut!(ls_response_stream);

                        while let Some(Ok(ls_response)) = ls_response_stream.next().await {
                            if let LsResponse::Success(entries) = ls_response {
                                for entry in entries.iter() {
                                    debug!(
                                        "Adding {} to wishlist dir: {}",
                                        entry.name, entry.is_dir
                                    );
                                }
                            }
                        }
                    }
                    Err(_) => {}
                }
                // file - download it
                // dir - query and download
            }
        };
        Ok(())
    }

    /// Turn a single request into potentially a set of requests to all peers
    async fn expand_request(&self, request: Request, name: String) -> Vec<(Request, String)> {
        if name.is_empty() {
            let peers = self.peers.lock().await;
            peers
                .keys()
                .map(|peer_name| (request.clone(), peer_name.to_string()))
                .collect()
        } else {
            vec![(request, name)]
        }
    }

    /// Open a request stream and write a request to the given peer
    async fn request(&self, request: Request, name: &str) -> Result<RecvStream, RequestError> {
        let peers = self.peers.lock().await;
        let connection = peers.get(name).ok_or(RequestError::PeerNotFound)?;
        let (mut send, recv) = connection.open_bi().await?;
        let buf = serialize(&request).map_err(|_| RequestError::SerializationError)?;
        debug!("message serialized, writing...");
        send.write_all(&buf).await?;
        send.finish().await?;
        debug!("message sent");
        Ok(recv)
    }
}

fn certificate_to_name(cert: Certificate) -> String {
    let mut hash = [0u8; 32];
    let mut topic_hash = Blake2b::new(32);
    topic_hash.input(cert.as_ref());
    topic_hash.result(&mut hash);
    key_to_animal::key_to_name(&hash)
}

fn send_event(sender: UnboundedSender<UiServerMessage>, event: UiEvent) {
    if sender.send(UiServerMessage::Event(event)).is_err() {
        warn!("UI response channel closed");
    }
}

async fn setup_download(file_path: PathBuf, start: Option<u64>) -> anyhow::Result<File> {
    create_dir_all(
        file_path
            .parent()
            .ok_or_else(|| anyhow!("Cannot get parent"))?,
    )
    .await?;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(file_path)
        .await?;
    if let Some(pos) = start {
        file.seek(std::io::SeekFrom::Start(pos)).await?;
    };
    Ok(file)
}

type LsResponseStream = BoxStream<'static, anyhow::Result<LsResponse>>;
async fn process_length_prefix(mut recv: RecvStream) -> anyhow::Result<LsResponseStream> {
    // Read the length prefix
    // TODO this should be a varint
    let mut length_buf: [u8; 8] = [0; 8];
    let stream = try_stream! {
        while let Ok(()) = recv.read_exact(&mut length_buf).await {
            let length: u64 = u64::from_le_bytes(length_buf);
            debug!("Read prefix {length}");

            // Read a message
            let mut msg_buf = vec![Default::default(); length.try_into().unwrap()];
            match recv.read_exact(&mut msg_buf).await {
                Ok(()) => {
                    let ls_response: LsResponse = deserialize(&msg_buf).unwrap();
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
}

// #[cfg(test)]
// mod tests {
//     use crate::wire_messages::Entry;
//
//     use super::*;
//     use tempfile::TempDir;
//
//     async fn setup_peer(share_dirs: Vec<&str>) -> (Hdp, UnboundedReceiver<UiServerMessage>) {
//         let storage = TempDir::new().unwrap();
//         Hdp::new(storage, share_dirs).await.unwrap()
//     }
//
//     #[tokio::test]
//     async fn test_read() -> Result<(), Box<dyn std::error::Error>> {
//         env_logger::init();
//         let (mut alice, _alice_rx) = setup_peer(vec!["tests/test-data"]).await;
//         let alice_addr = alice.endpoint.local_addr().unwrap();
//
//         let alice_command_tx = alice.command_tx.clone();
//         tokio::spawn(async move {
//             alice.run().await;
//         });
//
//         let (mut bob, mut bob_rx) = setup_peer(vec![]).await;
//         let bob_command_tx = bob.command_tx.clone();
//         tokio::spawn(async move {
//             bob.run().await;
//         });
//
//         // Connect to alice
//         bob_command_tx
//             .send(UiClientMessage {
//                 id: 0,
//                 command: Command::Connect(alice_addr),
//             })
//             .unwrap();
//
//         let _res = bob_rx.recv().await.unwrap();
//
//         // Do a read request
//         let req = Request::Read {
//             path: "test-data/somefile".to_string(),
//             start: None,
//             end: None,
//         };
//         bob_command_tx
//             .send(UiClientMessage {
//                 id: 1,
//                 command: Command::Request(req, alice_addr.to_string()),
//             })
//             .unwrap();
//
//         let res = bob_rx.recv().await.unwrap();
//         if let UiServerMessage::Response { id: _, response } = res {
//             assert_eq!(Ok(UiResponse::Read(b"boop\n".to_vec())), response);
//         } else {
//             panic!("Bad response");
//         }
//
//         // Do an Ls query
//         let req = Request::Ls {
//             path: None,
//             searchterm: None,
//             recursive: true,
//         };
//         bob_command_tx
//             .send(UiClientMessage {
//                 id: 1,
//                 command: Command::Request(req, alice_addr.to_string()),
//             })
//             .unwrap();
//
//         let mut entries = Vec::new();
//         while let UiServerMessage::Response {
//             id: _,
//             response: Ok(UiResponse::Ls(LsResponse::Success(some_entries), _name)),
//         } = bob_rx.recv().await.unwrap()
//         {
//             for entry in some_entries {
//                 entries.push(entry);
//             }
//         }
//         let test_entries = create_test_entries();
//         assert_eq!(test_entries, entries);
//
//         // Close the connection
//         alice_command_tx
//             .send(UiClientMessage {
//                 id: 3,
//                 command: Command::Close,
//             })
//             .unwrap();
//         Ok(())
//     }
//
//     fn create_test_entries() -> Vec<Entry> {
//         vec![
//             Entry {
//                 name: "".to_string(),
//                 size: 17,
//                 is_dir: true,
//             },
//             Entry {
//                 name: "test-data".to_string(),
//                 size: 17,
//                 is_dir: true,
//             },
//             Entry {
//                 name: "test-data/subdir".to_string(),
//                 size: 12,
//                 is_dir: true,
//             },
//             Entry {
//                 name: "test-data/subdir/subsubdir".to_string(),
//                 size: 6,
//                 is_dir: true,
//             },
//             Entry {
//                 name: "test-data/somefile".to_string(),
//                 size: 5,
//                 is_dir: false,
//             },
//             Entry {
//                 name: "test-data/subdir/anotherfile".to_string(),
//                 size: 6,
//                 is_dir: false,
//             },
//             Entry {
//                 name: "test-data/subdir/subsubdir/yetanotherfile".to_string(),
//                 size: 6,
//                 is_dir: false,
//             },
//         ]
//     }
// }
