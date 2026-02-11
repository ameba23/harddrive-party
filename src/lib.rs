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
        send.finish()?;
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
    #[error("Attempted to close an already closed stream")]
    ClosedStream(#[from] quinn::ClosedStream),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::discovery::DiscoveredPeer;
    use crate::ui_messages::{DownloadInfo, FilesQuery};
    use crate::wire_messages::{AnnouncePeer, Entry, ReadQuery, Request};
    use futures::StreamExt;
    use harddrive_party_shared::client::ClientError;
    use std::collections::HashSet;
    use tempfile::TempDir;
    use tokio::fs;
    use tokio::time::{timeout, Duration};

    fn init_logger() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    async fn setup_peer(share_dirs: Vec<String>) -> (Hdp, reqwest::Url) {
        let storage = TempDir::new().unwrap();
        let downloads = storage.path().to_path_buf();
        let hdp = Hdp::new(
            storage,
            share_dirs,
            downloads,
            false,
            Some("127.0.0.1:0".parse().unwrap()),
        )
        .await
        .unwrap();

        let http_server_addr =
            ui_server::http_server(hdp.shared_state.clone(), "127.0.0.1:0".parse().unwrap())
                .await
                .unwrap();
        let url = format!("http://{}", http_server_addr).parse().unwrap();
        (hdp, url)
    }

    async fn setup_connected_peers(
        share_dirs: Vec<String>,
    ) -> (
        SharedState,
        SharedState,
        ui_server::client::Client,
        ui_server::client::Client,
    ) {
        let (mut alice_hdp, alice_url) = setup_peer(share_dirs).await;
        let alice = alice_hdp.shared_state.clone();
        let alice_local_announce = alice.announce_address.clone();
        tokio::spawn(async move {
            alice_hdp.run().await;
        });

        let (mut bob_hdp, bob_url) = setup_peer(vec![]).await;
        let bob = bob_hdp.shared_state.clone();
        bob_hdp
            .shared_state
            .known_peers
            .add_peer(&alice_local_announce)
            .unwrap();
        bob_hdp
            .connect_to_peer(mock_discovered_peer(alice_local_announce))
            .await
            .unwrap();
        tokio::spawn(async move {
            bob_hdp.run().await;
        });

        let alice_client = ui_server::client::Client::new(alice_url);
        let bob_client = ui_server::client::Client::new(bob_url);

        (alice, bob, alice_client, bob_client)
    }

    async fn connect_peers(initiator: &mut Hdp, target: &SharedState) {
        initiator
            .shared_state
            .known_peers
            .add_peer(&target.announce_address)
            .unwrap();
        initiator
            .connect_to_peer(mock_discovered_peer(target.announce_address.clone()))
            .await
            .unwrap();
    }

    fn mock_discovered_peer(announce_address: AnnounceAddress) -> DiscoveredPeer {
        DiscoveredPeer {
            socket_address: format!(
                "127.0.0.1:{}",
                announce_address.connection_details.port().unwrap()
            )
            .parse()
            .unwrap(),
            socket_option: None,
            discovery_method: DiscoveryMethod::Direct,
            announce_address,
        }
    }

    #[tokio::test]
    async fn basic() {
        init_logger();
        let (alice, _bob, alice_client, bob_client) =
            setup_connected_peers(vec!["tests/test-data".to_string()]).await;

        let mut response_stream = alice_client
            .shares(IndexQuery {
                recursive: true,
                ..Default::default()
            })
            .await
            .unwrap();

        let mut response_entries = HashSet::new();
        while let Some(item) = response_stream.next().await {
            if let LsResponse::Success(entries) = item.unwrap() {
                for entry in entries {
                    response_entries.insert(entry);
                }
            }
        }
        assert_eq!(response_entries, create_test_entries());

        let query = FilesQuery {
            peer_name: None,
            query: IndexQuery {
                recursive: true,
                ..Default::default()
            },
        };
        let mut response_stream = bob_client.files(query).await.unwrap();

        let mut response_entries = HashSet::new();
        while let Some(item) = response_stream.next().await {
            if let (LsResponse::Success(entries), peer_name) = item.unwrap() {
                if peer_name == alice.name {
                    for entry in entries {
                        response_entries.insert(entry);
                    }
                }
            }
        }
        assert_eq!(response_entries, create_test_entries());

        let request_id = bob_client
            .download(&PeerPath {
                path: "test-data/somefile".to_string(),
                peer_name: alice.name,
            })
            .await
            .unwrap();

        let mut bob_events = bob_client.event_stream().await.unwrap();
        while let Some(event) = bob_events.next().await {
            if let Ok(UiEvent::Download(download_event)) = event {
                if let DownloadInfo::Completed(_) = download_event.download_info {
                    break;
                }
            }
        }

        let mut requested_files = bob_client.requested_files(request_id).await.unwrap();
        let requested_file = requested_files.next().await.unwrap().unwrap();
        assert_eq!(requested_file[0].path, "test-data/somefile");
    }

    #[tokio::test]
    async fn files_query_single_peer() {
        init_logger();
        let (alice, _bob, _alice_client, bob_client) =
            setup_connected_peers(vec!["tests/test-data".to_string()]).await;

        let query = FilesQuery {
            peer_name: Some(alice.name.clone()),
            query: IndexQuery {
                recursive: true,
                ..Default::default()
            },
        };
        let mut response_stream = bob_client.files(query).await.unwrap();

        let mut response_entries = HashSet::new();
        while let Some(item) = response_stream.next().await {
            if let (LsResponse::Success(entries), peer_name) = item.unwrap() {
                if peer_name == alice.name {
                    for entry in entries {
                        response_entries.insert(entry);
                    }
                }
            }
        }
        assert_eq!(response_entries, create_test_entries());
    }

    #[tokio::test]
    async fn files_query_searchterm() {
        init_logger();
        let (alice, _bob, _alice_client, bob_client) =
            setup_connected_peers(vec!["tests/test-data".to_string()]).await;

        let query = FilesQuery {
            peer_name: Some(alice.name.clone()),
            query: IndexQuery {
                searchterm: Some("somefile".to_string()),
                recursive: true,
                ..Default::default()
            },
        };
        let mut response_stream = bob_client.files(query).await.unwrap();

        let mut response_entries = HashSet::new();
        while let Some(item) = response_stream.next().await {
            if let (LsResponse::Success(entries), peer_name) = item.unwrap() {
                if peer_name == alice.name {
                    for entry in entries {
                        response_entries.insert(entry);
                    }
                }
            }
        }

        assert_eq!(
            response_entries,
            HashSet::from([Entry {
                name: "test-data/somefile".to_string(),
                size: 5,
                is_dir: false,
            }])
        );
    }

    #[tokio::test]
    async fn gossiped_peer_connection() {
        init_logger();
        let (mut alice_hdp, _alice_url) =
            setup_peer(vec!["tests/test-data".to_string()]).await;
        let alice = alice_hdp.shared_state.clone();
        tokio::spawn(async move {
            alice_hdp.run().await;
        });

        let (mut bob_hdp, _bob_url) = setup_peer(vec![]).await;
        let bob = bob_hdp.shared_state.clone();

        let (mut carol_hdp, _carol_url) = setup_peer(vec![]).await;
        let carol = carol_hdp.shared_state.clone();
        tokio::spawn(async move {
            carol_hdp.run().await;
        });

        // Bob connects to Alice and Carol
        connect_peers(&mut bob_hdp, &alice).await;
        connect_peers(&mut bob_hdp, &carol).await;
        tokio::spawn(async move {
            bob_hdp.run().await;
        });

        // Alice must trust Carol's cert for outgoing verification
        alice
            .known_peers
            .add_peer(&carol.announce_address)
            .unwrap();

        let mut alice_events = alice.event_broadcaster.subscribe();
        let announce_peer = AnnouncePeer {
            announce_address: carol.announce_address.clone(),
        };
        let _ = bob
            .request(Request::AnnouncePeer(announce_peer), &alice.name)
            .await
            .unwrap();

        let connected = timeout(Duration::from_secs(5), async move {
            while let Ok(event) = alice_events.recv().await {
                if let UiEvent::PeerConnected { name } = event {
                    if name == carol.name {
                        return true;
                    }
                }
            }
            false
        })
        .await
        .unwrap_or(false);

        assert!(connected, "Alice did not connect to Carol via gossip");
    }

    #[tokio::test]
    async fn uploaded_event_emitted_on_read() {
        init_logger();
        let (alice, bob, alice_client, bob_client) =
            setup_connected_peers(vec!["tests/test-data".to_string()]).await;

        let mut alice_events = alice_client.event_stream().await.unwrap();
        let mut read_stream = bob_client
            .read(
                alice.name.clone(),
                ReadQuery {
                    path: "test-data/somefile".to_string(),
                    start: None,
                    end: None,
                },
            )
            .await
            .unwrap();

        let read_task = tokio::spawn(async move {
            while let Some(Ok(_chunk)) = read_stream.next().await {}
        });

        let uploaded = timeout(Duration::from_secs(5), async move {
            while let Some(event) = alice_events.next().await {
                if let Ok(UiEvent::Uploaded(upload_info)) = event {
                    if upload_info.path == "test-data/somefile"
                        && upload_info.peer_name == bob.name
                    {
                        return true;
                    }
                }
            }
            false
        })
        .await
        .unwrap_or(false);

        let _ = read_task.await;

        assert!(uploaded, "Did not receive Uploaded event from Alice");
    }

    #[tokio::test]
    async fn ranged_read_returns_exact_requested_slice() {
        init_logger();
        let (alice, _bob, _alice_client, bob_client) =
            setup_connected_peers(vec!["tests/test-data".to_string()]).await;

        let path = "test-data/subdir/anotherfile".to_string();
        let start = 1_u64;
        let end = 3_u64;

        let mut read_stream = bob_client
            .read(
                alice.name.clone(),
                ReadQuery {
                    path: path.clone(),
                    start: Some(start),
                    end: Some(end),
                },
            )
            .await
            .unwrap();

        let mut received = Vec::new();
        while let Some(chunk) = read_stream.next().await {
            received.extend_from_slice(&chunk.unwrap());
        }

        let full = fs::read("tests/test-data/subdir/anotherfile")
            .await
            .unwrap();
        let expected = &full[start as usize..end as usize];

        assert_eq!(received, expected);
    }

    #[tokio::test]
    async fn add_share_dir() {
        let (mut alice_hdp, alice_url) = setup_peer(Vec::new()).await;
        tokio::spawn(async move {
            alice_hdp.run().await;
        });

        let alice_client = ui_server::client::Client::new(alice_url);

        let num_files_added = alice_client
            .add_share("tests/test-data".to_string())
            .await
            .unwrap();

        assert_eq!(num_files_added, 3);

        let mut response_stream = alice_client
            .shares(IndexQuery {
                recursive: true,
                ..Default::default()
            })
            .await
            .unwrap();

        let mut response_entries = HashSet::new();
        while let Some(item) = response_stream.next().await {
            if let LsResponse::Success(entries) = item.unwrap() {
                for entry in entries {
                    response_entries.insert(entry);
                }
            }
        }
        assert_eq!(response_entries, create_test_entries());

        alice_client
            .remove_share("test-data".to_string())
            .await
            .unwrap();

        let mut response_stream = alice_client
            .shares(IndexQuery {
                recursive: true,
                ..Default::default()
            })
            .await
            .unwrap();

        let mut response_entries = HashSet::new();
        while let Some(item) = response_stream.next().await {
            if let LsResponse::Success(entries) = item.unwrap() {
                for entry in entries {
                    response_entries.insert(entry);
                }
            }
        }

        assert_eq!(
            response_entries,
            HashSet::from([Entry {
                name: String::new(),
                size: 0,
                is_dir: true
            }])
        );

        assert_eq!(
            alice_client.remove_share("test-data".to_string()).await,
            Err(ClientError::ServerError(UiServerError::AddShare(
                "Share dir does not exist in DB".to_string()
            )))
        );
    }

    fn create_test_entries() -> HashSet<Entry> {
        HashSet::from([
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
        ])
    }
}
