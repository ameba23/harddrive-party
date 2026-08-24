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
    peer::{remote_path_is_within_request, safe_remote_relative_path, Peer},
    shares::Shares,
    ui_messages::{PeerPath, UiEvent, UiServerError},
    wire_messages::{AnnounceAddress, AnnouncePeer, Request},
    wishlist::{DownloadRequest, RequestedFile, WishList},
};
use async_stream::try_stream;
use futures::{pin_mut, StreamExt};
use harddrive_party_shared::{
    codec::{deserialize, serialize},
    wire_messages::{IndexQuery, LsResponse},
    PeerId,
};
use log::{debug, error, warn};
use quinn::RecvStream;
use rand::{rngs::OsRng, Rng};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc::Sender, oneshot, Mutex};

/// Maximum allowed payload size for a single length-prefixed peer message.
const MAX_LENGTH_PREFIX_MESSAGE_SIZE: u32 = 1024 * 1024;

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
    /// Legacy, name-keyed peer records. Kept untouched and deliberately ignored.
    pub const KNOWN_PEERS_LEGACY: &[u8; 1] = b"k";
    pub const KNOWN_PEERS_V1: &[u8; 2] = b"k1";
}

/// Shared state used by both the peer connections and user interface server
#[derive(Clone)]
pub struct SharedState {
    /// A map of canonical public-key IDs to active peer connections.
    pub peers: Arc<Mutex<HashMap<PeerId, Peer>>>,
    /// Persistent connection details keyed by exact public key.
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
    /// Our canonical public-key identity.
    pub id: PeerId,
    /// Our own connection details
    pub announce_address: AnnounceAddress,
    /// Our signed, gossip-authorized self-announcement.
    pub self_announcement: AnnouncePeer,
    /// Signature-verified remote announcements retained for live gossip only.
    pub verified_announcements: Arc<Mutex<HashMap<PeerId, AnnouncePeer>>>,
    /// Our OS home directory path
    pub os_home_dir: Option<String>,
    /// Whether graceful shutdown has started
    pub shutting_down: Arc<AtomicBool>,
    /// Peers intentionally disconnected until an explicit future connect call.
    pub(crate) manually_disconnected_peers: Arc<Mutex<HashSet<PeerId>>>,
    /// Peers with an outbound connect task currently in progress.
    pub(crate) pending_outbound_connections: Arc<Mutex<HashSet<PeerId>>>,
    /// Channel for graceful shutdown signal
    graceful_shutdown_tx: tokio::sync::mpsc::Sender<()>,
}

impl SharedState {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        db: sled::Db,
        share_dirs: Vec<String>,
        download_dir: PathBuf,
        id: PeerId,
        name: String,
        peer_announce_tx: Sender<PeerConnect>,
        peers: Arc<Mutex<HashMap<PeerId, Peer>>>,
        announce_address: AnnounceAddress,
        self_announcement: AnnouncePeer,
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
            id,
            name,
            announce_address,
            self_announcement,
            verified_announcements: Default::default(),
            os_home_dir,
            shutting_down: Arc::new(AtomicBool::new(false)),
            manually_disconnected_peers: Default::default(),
            pending_outbound_connections: Default::default(),
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
    pub async fn request(&self, request: Request, id: &PeerId) -> Result<RecvStream, RequestError> {
        let connection = {
            let peers = self.peers.lock().await;
            let peer = peers.get(id).ok_or(RequestError::PeerNotFound)?;
            peer.connection.clone()
        };
        Self::request_connection(request, &connection).await
    }

    /// Static method to open a request stream and write a request to the given peer
    pub async fn request_peer(request: Request, peer: &Peer) -> Result<RecvStream, RequestError> {
        Self::request_connection(request, &peer.connection).await
    }

    /// Static method to open a request stream and write a request on the given connection
    pub async fn request_connection(
        request: Request,
        connection: &quinn::Connection,
    ) -> Result<RecvStream, RequestError> {
        let (mut send, recv) = connection.open_bi().await?;
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
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(UiServerError::ConnectionError(
                "Shutting down; not connecting to peer".to_string(),
            )
            .into());
        }
        let peer_id = announce_address.public_key;
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
        response_rx.await??;
        self.manually_disconnected_peers
            .lock()
            .await
            .remove(&peer_id);
        Ok(())
    }

    /// Intentionally disconnect from a connected peer and suppress automatic reconnects until an
    /// explicit future connect call.
    pub async fn disconnect_peer(&self, peer_id: &PeerId) -> Result<(), UiServerErrorWrapper> {
        let connection = {
            let peers = self.peers.lock().await;
            let peer = peers
                .get(peer_id)
                .ok_or_else(|| UiServerError::ConnectionError("Peer not connected".to_string()))?;
            peer.connection.clone()
        };

        self.manually_disconnected_peers
            .lock()
            .await
            .insert(*peer_id);
        connection.close(0u32.into(), b"disconnect");
        Ok(())
    }

    pub async fn download(&self, peer_path: PeerPath) -> Result<u32, UiServerErrorWrapper> {
        let requested_path = safe_remote_relative_path(&peer_path.path).map_err(|err| {
            UiServerError::RequestError(format!("Invalid download path requested: {err}"))
        })?;

        // Get details of the file / dir
        let ls_request = Request::Ls(IndexQuery {
            path: Some(peer_path.path.clone()),
            searchterm: None,
            recursive: true,
        });

        let recv = self.request(ls_request, &peer_path.peer.id).await?;

        let peer_public_key = {
            let peers = self.peers.lock().await;
            match peers.get(&peer_path.peer.id) {
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
                    let entry_path = match safe_remote_relative_path(&entry.name) {
                        Ok(path) => path,
                        Err(err) => {
                            warn!(
                                "Ignoring unsafe download entry from peer {}: {err}",
                                peer_path.peer.name
                            );
                            continue;
                        }
                    };

                    if !remote_path_is_within_request(&requested_path, &entry_path) {
                        warn!(
                            "Ignoring download entry outside requested path from peer {}: {}",
                            peer_path.peer.name, entry.name
                        );
                        continue;
                    }

                    if entry.name == peer_path.path {
                        if let Err(err) = self.wishlist.add_request(&DownloadRequest::new(
                            entry.name.clone(),
                            entry.size,
                            id,
                            peer_public_key.into_bytes(),
                            entry.is_dir,
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
        self.shutting_down.store(true, Ordering::SeqCst);
        // TODO tidy up peer discovery / active transfers
        self.shares.flush().await;
        self.wishlist.flush().await;
        // This sends a signal to shutdown the Quic endpoint
        if self.graceful_shutdown_tx.try_send(()).is_err() {
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
            if length > MAX_LENGTH_PREFIX_MESSAGE_SIZE {
                Err(anyhow::anyhow!(
                    "Message too large: {length} > {MAX_LENGTH_PREFIX_MESSAGE_SIZE}"
                ))?;
            }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::discovery::stun::test_utils::spawn_mock_stun_server;
    use crate::ui_messages::{DownloadInfo, FilesQuery};
    use crate::wire_messages::{Entry, ReadQuery};
    use futures::StreamExt;
    use harddrive_party_shared::client::ClientError;
    use std::{collections::HashSet, net::UdpSocket as StdUdpSocket};
    use tempfile::TempDir;
    use tokio::fs;
    use tokio::time::{timeout, Duration};

    fn init_logger() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    async fn setup_peer(share_dirs: Vec<String>) -> (Hdp, reqwest::Url) {
        let storage = TempDir::new().unwrap();
        let downloads = storage.path().to_path_buf();
        let (stun_server_1, stun_handle_1) = spawn_mock_stun_server(None).await;
        let (stun_server_2, stun_handle_2) = spawn_mock_stun_server(None).await;
        let hdp = Hdp::new(
            storage,
            share_dirs,
            downloads,
            false,
            Some("127.0.0.1:0".parse().unwrap()),
            Some(vec![stun_server_1, stun_server_2]),
        )
        .await
        .unwrap();
        stun_handle_1.abort();
        stun_handle_2.abort();

        let http_server_addr =
            ui_server::http_server(hdp.shared_state.clone(), "127.0.0.1:0".parse().unwrap())
                .await
                .unwrap();
        let url = format!("http://{}", http_server_addr).parse().unwrap();
        (hdp, url)
    }

    #[tokio::test]
    async fn local_address_requires_configured_address() {
        let storage = TempDir::new().unwrap();
        let downloads = storage.path().to_path_buf();
        let held_socket = StdUdpSocket::bind("127.0.0.1:0").unwrap();
        let held_addr = held_socket.local_addr().unwrap();

        let result = Hdp::new(storage, vec![], downloads, false, Some(held_addr), None).await;
        let err = match result {
            Ok(_) => panic!("expected configured QUIC address bind to fail"),
            Err(err) => err,
        };

        assert!(
            err.to_string()
                .contains("failed to bind configured QUIC UDP address"),
            "unexpected error: {err}"
        );
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

        bob.connect_to_peer(alice_local_announce).await.unwrap();
        tokio::spawn(async move {
            bob_hdp.run().await;
        });

        wait_for_peer_presence(&bob, alice.id, true).await;

        let alice_client = ui_server::client::Client::new(alice_url);
        let bob_client = ui_server::client::Client::new(bob_url);

        (alice, bob, alice_client, bob_client)
    }

    async fn wait_for_peer_presence(shared_state: &SharedState, peer_id: PeerId, present: bool) {
        let observed = timeout(Duration::from_secs(30), async {
            loop {
                if shared_state.peers.lock().await.contains_key(&peer_id) == present {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;

        assert!(
            observed.is_ok(),
            "Timed out waiting for peer {peer_id} presence to become {present}"
        );
    }

    async fn wait_for_pending_outbound_presence(
        shared_state: &SharedState,
        peer_id: PeerId,
        present: bool,
    ) {
        let observed = timeout(Duration::from_secs(5), async {
            loop {
                if shared_state
                    .pending_outbound_connections
                    .lock()
                    .await
                    .contains(&peer_id)
                    == present
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;

        assert!(
            observed.is_ok(),
            "Timed out waiting for pending outbound presence for {peer_id} to become {present}"
        );
    }

    async fn wait_for_signed_announcement(shared_state: &SharedState, peer_id: PeerId) {
        let observed = timeout(Duration::from_secs(5), async {
            loop {
                if shared_state
                    .peers
                    .lock()
                    .await
                    .get(&peer_id)
                    .is_some_and(|peer| {
                        peer.announcement
                            .as_ref()
                            .is_some_and(|announcement| announcement.verify())
                    })
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            observed.is_ok(),
            "Timed out waiting for signed announcement from {peer_id}"
        );
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
            peer_id: None,
            query: IndexQuery {
                recursive: true,
                ..Default::default()
            },
        };
        let mut response_stream = bob_client.files(query).await.unwrap();

        let mut response_entries = HashSet::new();
        while let Some(item) = response_stream.next().await {
            if let (LsResponse::Success(entries), peer) = item.unwrap() {
                if peer.id == alice.id {
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
                peer: harddrive_party_shared::ui_messages::PeerInfo::from_id(alice.id),
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
            peer_id: Some(alice.id),
            query: IndexQuery {
                recursive: true,
                ..Default::default()
            },
        };
        let mut response_stream = bob_client.files(query).await.unwrap();

        let mut response_entries = HashSet::new();
        while let Some(item) = response_stream.next().await {
            if let (LsResponse::Success(entries), peer) = item.unwrap() {
                if peer.id == alice.id {
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
            peer_id: Some(alice.id),
            query: IndexQuery {
                searchterm: Some("somefile".to_string()),
                recursive: true,
                ..Default::default()
            },
        };
        let mut response_stream = bob_client.files(query).await.unwrap();

        let mut response_entries = HashSet::new();
        while let Some(item) = response_stream.next().await {
            if let (LsResponse::Success(entries), peer) = item.unwrap() {
                if peer.id == alice.id {
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
        // Setup 3 peers
        let (mut alice_hdp, _alice_url) = setup_peer(vec!["tests/test-data".to_string()]).await;
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
        bob.connect_to_peer(alice.announce_address).await.unwrap();
        bob.connect_to_peer(carol.announce_address.clone())
            .await
            .unwrap();
        tokio::spawn(async move {
            bob_hdp.run().await;
        });

        // Wait until Alice's authoritative peer map includes Carol.
        // This avoids races where a broadcast event is emitted before we subscribe.
        let carol_id = carol.id;
        let connected = timeout(Duration::from_secs(5), async move {
            loop {
                if alice.peers.lock().await.contains_key(&carol_id) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or(false);

        assert!(connected, "Alice did not connect to Carol via gossip");
    }

    #[tokio::test]
    async fn both_connection_directions_exchange_signed_self_announcements() {
        init_logger();
        let (alice, bob, _alice_client, _bob_client) = setup_connected_peers(vec![]).await;

        wait_for_signed_announcement(&alice, bob.id).await;
        wait_for_signed_announcement(&bob, alice.id).await;
        assert_eq!(
            alice
                .peers
                .lock()
                .await
                .get(&bob.id)
                .unwrap()
                .announcement
                .as_ref()
                .unwrap()
                .peer_id(),
            bob.id
        );
    }

    #[tokio::test]
    async fn invalid_third_party_gossip_is_dropped_without_closing_connection() {
        init_logger();
        let (alice, bob, _alice_client, _bob_client) = setup_connected_peers(vec![]).await;
        wait_for_signed_announcement(&alice, bob.id).await;
        wait_for_signed_announcement(&bob, alice.id).await;

        let connection = bob
            .peers
            .lock()
            .await
            .get(&alice.id)
            .unwrap()
            .connection
            .clone();
        let forged_id = PeerId::new([88; 32]);
        let forged = wire_messages::AnnouncePeer {
            announce_address: AnnounceAddress {
                public_key: forged_id,
                connection_details: wire_messages::PeerConnectionDetails::NoNat(
                    "203.0.113.88:7088".parse().unwrap(),
                ),
            },
            signature: [0; 64],
        };
        assert!(!forged.verify());

        let _recv = SharedState::request_connection(Request::AnnouncePeer(forged), &connection)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(!alice.known_peers.has(&forged_id));
        assert!(connection.close_reason().is_none());
        assert!(alice.peers.lock().await.contains_key(&bob.id));
    }

    #[tokio::test]
    async fn disconnect_peer_suppresses_reconnect_until_explicit_connect() {
        init_logger();
        let (alice, bob, _alice_client, bob_client) = setup_connected_peers(vec![]).await;

        wait_for_peer_presence(&bob, alice.id, true).await;

        bob_client.disconnect(alice.id).await.unwrap();

        wait_for_peer_presence(&bob, alice.id, false).await;

        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !bob.peers.lock().await.contains_key(&alice.id),
            "Peer reconnected after intentional disconnect"
        );

        let request_err = bob
            .request(
                Request::Ls(IndexQuery {
                    recursive: false,
                    ..Default::default()
                }),
                &alice.id,
            )
            .await
            .unwrap_err();
        assert!(matches!(request_err, RequestError::PeerNotFound));

        bob.connect_to_peer(alice.announce_address.clone())
            .await
            .unwrap();
        wait_for_peer_presence(&bob, alice.id, true).await;
    }

    #[tokio::test]
    async fn incoming_connections_are_processed_while_outbound_connect_retries() {
        init_logger();

        let (mut alice_hdp, _alice_url) = setup_peer(vec![]).await;
        let alice = alice_hdp.shared_state.clone();
        tokio::spawn(async move {
            alice_hdp.run().await;
        });

        let unreachable_addr = StdUdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let unreachable_peer = AnnounceAddress {
            public_key: PeerId::new([42; 32]),
            connection_details: wire_messages::PeerConnectionDetails::NoNat(unreachable_addr),
        };

        alice.connect_to_peer(unreachable_peer).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let (mut bob_hdp, _bob_url) = setup_peer(vec![]).await;
        let bob = bob_hdp.shared_state.clone();
        bob.connect_to_peer(alice.announce_address.clone())
            .await
            .unwrap();
        tokio::spawn(async move {
            bob_hdp.run().await;
        });

        let observed = timeout(Duration::from_secs(5), async {
            loop {
                if alice.peers.lock().await.contains_key(&bob.id) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;

        assert!(
            observed.is_ok(),
            "Timed out waiting for inbound connection while another outbound connect was retrying"
        );
    }

    #[tokio::test]
    async fn duplicate_outbound_connect_announcements_are_deduplicated_while_retrying() {
        init_logger();

        let (mut alice_hdp, _alice_url) = setup_peer(vec![]).await;
        let alice = alice_hdp.shared_state.clone();
        tokio::spawn(async move {
            alice_hdp.run().await;
        });

        let unreachable_addr = StdUdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let unreachable_peer = AnnounceAddress {
            public_key: PeerId::new([43; 32]),
            connection_details: wire_messages::PeerConnectionDetails::NoNat(unreachable_addr),
        };

        alice
            .connect_to_peer(unreachable_peer.clone())
            .await
            .unwrap();
        wait_for_pending_outbound_presence(&alice, unreachable_peer.public_key, true).await;

        alice
            .connect_to_peer(unreachable_peer.clone())
            .await
            .unwrap();
        alice
            .connect_to_peer(unreachable_peer.clone())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let pending = alice.pending_outbound_connections.lock().await;
        assert_eq!(
            pending.len(),
            1,
            "expected only one pending outbound connect task after duplicate announcements"
        );
        assert!(
            pending.contains(&unreachable_peer.public_key),
            "expected duplicate announcements to keep exactly one pending outbound connect"
        );
    }

    #[tokio::test]
    async fn failed_explicit_connect_does_not_set_manual_reconnect_state() {
        init_logger();

        let (alice, bob, _alice_client, _bob_client) = setup_connected_peers(vec![]).await;

        let err = bob
            .connect_to_peer(alice.announce_address.clone())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Already connected to this peer"),
            "unexpected error: {err}"
        );

        let manually_disconnected = bob
            .manually_disconnected_peers
            .lock()
            .await
            .contains(&alice.id);
        assert!(
            !manually_disconnected,
            "failed explicit connect should not leave manual reconnect state behind"
        );
    }

    #[tokio::test]
    async fn failed_explicit_connect_does_not_block_future_auto_reconnect() {
        init_logger();

        let (alice, bob, _alice_client, _bob_client) = setup_connected_peers(vec![]).await;

        let err = bob
            .connect_to_peer(alice.announce_address.clone())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Already connected to this peer"),
            "unexpected error: {err}"
        );

        let connection = {
            let peers = bob.peers.lock().await;
            peers.get(&alice.id).unwrap().connection.clone()
        };
        let original_stable_id = connection.stable_id();
        connection.close(0u32.into(), b"test disconnect");

        let peer_id = alice.id;
        let reconnected = timeout(Duration::from_secs(30), async {
            loop {
                let reconnected = {
                    let peers = bob.peers.lock().await;
                    peers
                        .get(&peer_id)
                        .is_some_and(|peer| peer.connection.stable_id() != original_stable_id)
                };
                if reconnected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;

        assert!(
            reconnected.is_ok(),
            "Timed out waiting for auto reconnect to replace the closed connection"
        );
    }

    #[tokio::test]
    async fn uploaded_event_emitted_on_read() {
        init_logger();
        let (alice, bob, alice_client, bob_client) =
            setup_connected_peers(vec!["tests/test-data".to_string()]).await;

        let mut alice_events = alice_client.event_stream().await.unwrap();
        let mut read_stream = bob_client
            .read(
                alice.id,
                ReadQuery {
                    path: "test-data/somefile".to_string(),
                    start: None,
                    end: None,
                },
            )
            .await
            .unwrap();

        let read_task =
            tokio::spawn(async move { while let Some(Ok(_chunk)) = read_stream.next().await {} });

        let uploaded = timeout(Duration::from_secs(15), async move {
            while let Some(event) = alice_events.next().await {
                if let Ok(UiEvent::Uploaded(upload_info)) = event {
                    if upload_info.path == "test-data/somefile" && upload_info.peer.id == bob.id {
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
                alice.id,
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
