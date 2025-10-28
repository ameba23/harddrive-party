pub mod connections;
pub mod errors;
pub mod peer;
pub mod shares;
pub mod ui_server;
pub mod wishlist;

pub use connections::Hdp;
pub use harddrive_party_shared::ui_messages;
pub use harddrive_party_shared::wire_messages;

use crate::{
    connections::{
        discovery::{DiscoveryMethod, PeerConnect},
        known_peers::KnownPeers,
    },
    errors::UiServerErrorWrapper,
    peer::Peer,
    shares::Shares,
    ui_messages::{PeerPath, UiEvent, UiServerError},
    wire_messages::{AnnounceAddress, Request},
    wishlist::{DownloadRequest, RequestedFile, WishList},
};
use async_stream::try_stream;
use bincode::serialize;
use futures::{pin_mut, StreamExt};
use harddrive_party_shared::wire_messages::{IndexQuery, LsResponse};
use log::{debug, error, warn};
use quinn::RecvStream;
use rand::{rngs::OsRng, Rng};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc::Sender, oneshot, Mutex};

/// Key-value store sub-tree names
pub mod subtree_names {
    pub const CONFIG: &[u8; 1] = b"c";
    pub const FILES: &[u8; 1] = b"f";
    pub const DIRS: &[u8; 1] = b"d";
    pub const SHARE_NAMES: &[u8; 1] = b"s";
    pub const REQUESTS: &[u8; 1] = b"r";
    pub const REQUESTS_BY_TIMESTAMP: &[u8; 1] = b"R";
    pub const REQUESTS_PROGRESS: &[u8; 1] = b"P";
    pub const REQUESTED_FILES_BY_PEER: &[u8; 1] = b"p";
    pub const REQUESTED_FILES_BY_REQUEST_ID: &[u8; 1] = b"C";
    pub const KNOWN_PEERS: &[u8; 1] = b"k";
}

/// Shared state used by both the peer connections and user interface server
#[derive(Clone)]
pub struct SharedState {
    /// A map of peer names to active peer connections
    pub peers: Arc<Mutex<HashMap<String, Peer>>>,
    /// A list of known peer names
    pub known_peers: KnownPeers,
    /// The index of shared files
    pub shares: Shares,
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
    /// Our OS home directory path
    pub os_home_dir: Option<String>,
    /// Channel for graceful shutdown signal
    graceful_shutdown_tx: tokio::sync::mpsc::Sender<()>,
}

impl SharedState {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        db: sled::Db,
        share_dirs: Vec<String>,
        download_dir: PathBuf,
        name: String,
        peer_announce_tx: Sender<PeerConnect>,
        peers: Arc<Mutex<HashMap<String, Peer>>>,
        announce_address: AnnounceAddress,
        graceful_shutdown_tx: tokio::sync::mpsc::Sender<()>,
        known_peers: KnownPeers,
    ) -> anyhow::Result<Self> {
        let shares = Shares::new(db.clone(), share_dirs).await?;

        // Set home dir - this is used in the UI as a placeholder when choosing a directory to
        // share
        // TODO for cross platform support we should use the `home` crate
        let os_home_dir = match std::env::var_os("HOME") {
            Some(o) => o.to_str().map(|s| s.to_string()),
            None => None,
        };

        // For sending events to UI clients over websocket
        let (event_broadcaster, _rx) = broadcast::channel(65536);

        Ok(Self {
            peers,
            known_peers,
            shares,
            wishlist: WishList::new(&db)?,
            event_broadcaster,
            peer_announce_tx,
            download_dir,
            name,
            announce_address,
            os_home_dir,
            graceful_shutdown_tx,
        })
    }

    /// Send an event to the UI
    pub async fn send_event(&self, event: UiEvent) {
        if self.event_broadcaster.send(event).is_err() {
            warn!("UI response channel closed");
        }
    }

    /// Open a request stream and write a request to the peer with the given name
    pub async fn request(&self, request: Request, name: &str) -> Result<RecvStream, RequestError> {
        let peers = self.peers.lock().await;
        let peer = peers.get(name).ok_or(RequestError::PeerNotFound)?;
        Self::request_peer(request, peer).await
    }

    /// Static method to open a request stream and write a request to the given peer
    pub async fn request_peer(request: Request, peer: &Peer) -> Result<RecvStream, RequestError> {
        let (mut send, recv) = peer.connection.open_bi().await?;
        let buf = serialize(&request).map_err(|_| RequestError::SerializationError)?;
        debug!("Message serialized, writing...");
        send.write_all(&buf).await?;
        send.finish().unwrap(); // TODO
        debug!("Message sent");
        Ok(recv)
    }

    pub fn get_ui_announce_address(&self) -> String {
        self.announce_address.to_string()
    }

    pub async fn connect_to_peer(
        &self,
        announce_address: AnnounceAddress,
    ) -> Result<(), UiServerErrorWrapper> {
        let discovery_method = DiscoveryMethod::Direct;

        let (response_tx, response_rx) = oneshot::channel();
        let peer_connect = PeerConnect {
            discovery_method,
            announce_address,
            response_tx: Some(response_tx),
        };
        self.peer_announce_tx
            .send(peer_connect)
            .await
            .map_err(|_| {
                UiServerError::PeerDiscovery("Peer announce channel closed".to_string())
            })?;

        // TODO this could take a very long time as the other peer may not show up
        // add a timeout here
        response_rx.await?
    }

    pub async fn download(&self, peer_path: PeerPath) -> Result<u32, UiServerErrorWrapper> {
        // Get details of the file / dir
        let ls_request = Request::Ls(IndexQuery {
            path: Some(peer_path.path.clone()),
            searchterm: None,
            recursive: true,
        });
        //             // let mut cache = self.ls_cache.lock().await;
        //             //
        //             // if let hash_map::Entry::Occupied(mut peer_cache_entry) =
        //             //     cache.entry(peer_name.clone())
        //             // {
        //             //     let peer_cache = peer_cache_entry.get_mut();
        //             //     if let Some(responses) = peer_cache.get(&ls_request) {
        //             //         debug!("Found existing responses in cache");
        //             //         for entries in responses.iter() {
        //             //             for entry in entries.iter() {
        //             //                 debug!("Adding {} to wishlist dir: {}", entry.name, entry.is_dir);
        //             //             }
        //             //         }
        //             //     } else {
        //             //         debug!("Found nothing in cache");
        //             //     }
        //             // }
        //
        let recv = self.request(ls_request, &peer_path.peer_name).await?;

        let peer_public_key = {
            let peers = self.peers.lock().await;
            match peers.get(&peer_path.peer_name) {
                Some(peer) => peer.public_key,
                None => {
                    warn!("Handling request to download a file from a peer who is not connected");
                    // TODO return an error
                    return Err(
                        UiServerError::ConnectionError("Peer not connected".to_string()).into(),
                    );
                }
            }
        };
        let mut rng = OsRng;
        let id: u32 = rng.gen();

        let ls_response_stream = process_length_prefix(recv).await?;
        pin_mut!(ls_response_stream);
        while let Some(Ok(ls_response)) = ls_response_stream.next().await {
            if let LsResponse::Success(entries) = ls_response {
                for entry in entries.iter() {
                    if entry.name == peer_path.path {
                        if let Err(err) = self.wishlist.add_request(&DownloadRequest::new(
                            entry.name.clone(),
                            entry.size,
                            id,
                            peer_public_key,
                        )) {
                            error!("Cannot add download request {err:?}");
                        }
                    }
                    if !entry.is_dir {
                        debug!("Adding {} to wishlist", entry.name);

                        if let Err(err) = self.wishlist.add_requested_file(&RequestedFile {
                            path: entry.name.clone(),
                            size: entry.size,
                            request_id: id,
                            downloaded: false,
                        }) {
                            error!("Cannot make download request {err:?}");
                        };
                    }
                }
            }
        }
        Ok(id)
    }

    /// Gracefully shut down the process
    pub async fn shut_down(&self) {
        // TODO tidy up peer discovery / active transfers
        self.shares.flush().await;
        self.wishlist.flush().await;
        // This sends a signal to shutdown the Quic endpoint
        if self.graceful_shutdown_tx.send(()).await.is_err() {
            std::process::exit(0);
        };
    }
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

/// A stream of Ls responses
pub type LsResponseStream = futures::stream::BoxStream<'static, anyhow::Result<LsResponse>>;

/// Process responses from a remote peer that are prefixed with their length in bytes
pub async fn process_length_prefix(
    mut recv: quinn::RecvStream,
) -> Result<LsResponseStream, UiServerErrorWrapper> {
    // Read the length prefix
    let mut length_buf: [u8; 4] = [0; 4];
    let stream = try_stream! {
        while let Ok(()) = recv.read_exact(&mut length_buf).await {
            let length: u32 = u32::from_be_bytes(length_buf);
            debug!("Read prefix {length}");

            // Read a message
            let length_usize: usize = length.try_into()?;
            let mut msg_buf = vec![Default::default(); length_usize];
            match recv.read_exact(&mut msg_buf).await {
                Ok(()) => {
                    let ls_response: LsResponse = bincode::deserialize(&msg_buf)?;
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
