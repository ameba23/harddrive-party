//! Remote procedure call for share index queries and file uploading

use crate::{
    connections::{
        discovery::{DiscoveryMethod, PeerConnect},
        known_peers::KnownPeers,
    },
    peer::Peer,
    shares::{EntryParseError, Shares},
    ui_messages::{UiEvent, UploadInfo},
    SharedState,
};
use futures::FutureExt;
use harddrive_party_shared::{
    codec::{deserialize, serialize, DecodeError},
    wire_messages::{AnnouncePeer, IndexQuery, LsResponse, LsResponseError, ReadQuery, Request},
};
use log::{debug, error, info, warn};
use quinn::WriteError;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt},
    sync::{broadcast, mpsc::Sender, Mutex},
};

use super::{
    speedometer::{self, Speedometer},
    uploads::{self, UploadSender},
};

/// Number of bytes uploaded at a time
const UPLOAD_BLOCK_SIZE: usize = 64 * 1024;
/// Minimum bytes written before emitting upload progress.
const MIN_UPLOAD_UI_UPDATE_BYTES: u64 = 256 * 1024;
/// Minimum time between upload progress updates on fast links.
const MIN_UPLOAD_UI_UPDATE_INTERVAL: Duration = Duration::from_millis(300);
/// Maximum time between upload progress updates on slow links.
const MAX_UPLOAD_UI_UPDATE_INTERVAL: Duration = Duration::from_secs(2);

struct ReadRequest {
    path: String,
    start: Option<u64>,
    end: Option<u64>,
    output: quinn::SendStream,
    requester_name: String,
}

/// Remote Procedure Call - process remote requests
#[derive(Clone)]
pub struct Rpc {
    /// The file index database
    pub shares: Shares,
    /// Channel for sending upload requests
    upload_tx: UploadSender<ReadRequest>,
    peer_announce_tx: Sender<PeerConnect>,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    known_peers: KnownPeers,
}

impl Rpc {
    pub fn new(
        shares: Shares,
        event_broadcaster: broadcast::Sender<UiEvent>,
        peer_announce_tx: Sender<PeerConnect>,
        peers: Arc<Mutex<HashMap<String, Peer>>>,
        known_peers: KnownPeers,
    ) -> Rpc {
        let uploader = Uploader {
            shares: shares.clone(),
            event_broadcaster,
        };
        let (upload_tx, _) = uploads::spawn(move |request: ReadRequest| {
            let uploader = uploader.clone();
            async move { uploader.upload(request).await }
        });

        Rpc {
            shares,
            upload_tx,
            peer_announce_tx,
            peers,
            known_peers,
        }
    }

    /// Handle a request
    pub async fn request(&self, buf: Vec<u8>, output: quinn::SendStream, peer_name: String) {
        let request: Result<Request, DecodeError> = deserialize(&buf);
        match request {
            Ok(req) => {
                debug!("Got request from peer {req:?}");
                match req {
                    Request::Ls(IndexQuery {
                        path,
                        searchterm,
                        recursive,
                    }) => {
                        if let Err(err) = self.ls(path, searchterm, recursive, output).await {
                            warn!("Error handling incoming index query {err}");
                        };
                    }
                    Request::Read(ReadQuery { path, start, end }) => {
                        if self
                            .read(path, start, end, output, peer_name)
                            .await
                            .is_err()
                        {
                            warn!("Could not send read request - channel closed");
                        }
                    }
                    Request::AnnouncePeer(announce_peer) => {
                        if announce_peer.announce_address.name == peer_name {
                            info!("Received self-announcement {announce_peer:?}");
                            self.handle_self_announcement(&announce_peer, &peer_name)
                                .await;
                        } else {
                            info!(
                                "Received gossiped peer announcement from {} about {} ({:?})",
                                peer_name,
                                announce_peer.announce_address.name,
                                announce_peer.announce_address.connection_details
                            );
                            if self
                                .peer_announce_tx
                                .send(PeerConnect {
                                    discovery_method: DiscoveryMethod::Gossip,
                                    announce_address: announce_peer.announce_address,
                                    response_tx: None,
                                })
                                .await
                                .is_err()
                            {
                                error!("Unable to announce peer - channel closed");
                            }
                        }
                    }
                }
            }
            Err(_) => {
                warn!("Cannot decode wire message");
            }
        }
    }

    /// Handle a self-announcement and relay it to existing peers
    async fn handle_self_announcement(&self, announce_peer: &AnnouncePeer, sender: &str) {
        let other_connections = {
            let mut peers = self.peers.lock().await;

            // Add the announce details to the Peer struct
            if let Some(sender_peer) = peers.get_mut(sender) {
                sender_peer.announce_address = Some(announce_peer.announce_address.clone());
            } else {
                warn!("Got self-announcement from peer we are not connected to");
            }

            // Snapshot peers so we don't hold locks while awaiting network sends
            peers
                .iter()
                .filter(|(other_name, _)| other_name.as_str() != sender)
                .map(|(_, peer)| peer.connection.clone())
                .collect::<Vec<_>>()
        };

        // Also add it to the known peers list
        if let Err(err) = self.known_peers.add_peer(&announce_peer.announce_address) {
            warn!("Could not persist announce details for {sender}: {err}");
        }

        // Tell all existing peers about this sender
        for other_connection in other_connections {
            info!(
                "Relaying self-announcement for {} to an existing peer connection",
                sender
            );
            let request = Request::AnnouncePeer(announce_peer.clone());
            if let Err(err) = SharedState::request_connection(request, &other_connection).await {
                error!("Failed to relay announce message to peer connection: {err:?}");
            }
        }
    }

    /// Query the filepath index
    async fn ls(
        &self,
        path: Option<String>,
        searchterm: Option<String>,
        recursive: bool,
        mut output: quinn::SendStream,
    ) -> Result<(), RpcError> {
        match self.shares.query(path, searchterm, recursive) {
            Ok(response_iterator) => {
                for res in response_iterator {
                    let buf = serialize(&res).map_err(|e| {
                        error!("Cannot serialize query response {e:?}");
                        RpcError::SerializeError
                    })?;

                    // Write the length prefix
                    // TODO use big endian, u32
                    let length: u32 = buf
                        .len()
                        .try_into()
                        .map_err(|_| RpcError::U64ConvertError)?;
                    debug!("Writing prefix {length}");
                    output.write_all(&length.to_be_bytes()).await?;

                    output.write_all(&buf).await?;
                    debug!("Written ls response");
                }
                output.finish()?;
                Ok(())
            }
            Err(error) => {
                warn!("Error during share query {error:?}");
                send_ls_error(
                    match error {
                        EntryParseError::PathNotFound => RpcError::PathNotFound,
                        _ => RpcError::DbError,
                    },
                    output,
                )
                .await
            }
        }
    }

    /// Read a portion of a file
    /// This puts the read request, which contains a stream for the response, in a queue
    async fn read(
        &self,
        path: String,
        start: Option<u64>,
        end: Option<u64>,
        output: quinn::SendStream,
        requester_name: String,
    ) -> Result<(), RpcError> {
        let stopped = output.stopped();
        let request = ReadRequest {
            path,
            start,
            end,
            output,
            requester_name: requester_name.clone(),
        };
        tokio::select! {
            result = self.upload_tx.send(requester_name, request) => {
                result.map_err(|_| RpcError::ChannelClosed)
            },
            _ = stopped => Ok(()),
        }
    }
}

/// Resources shared by the bounded concurrent upload tasks.
#[derive(Clone)]
struct Uploader {
    /// Our share db
    shares: Shares,
    /// Channel for sending messages to the UI
    event_broadcaster: broadcast::Sender<UiEvent>,
}

impl Uploader {
    async fn upload(&self, request: ReadRequest) -> Result<(), RpcError> {
        // Skip already canceled requests before opening their files.
        if request.output.stopped().now_or_never().is_some() {
            return Ok(());
        }
        let stopped = request.output.stopped();
        tokio::select! {
            result = self.do_read(request) => result,
            _ = stopped => Ok(()),
        }
    }

    /// Upload a file portion
    async fn do_read(&self, read_request: ReadRequest) -> Result<(), RpcError> {
        // Possibly here we could adjust the block size based on conjestion
        // by using output.write and checking the number of bytes written in one go

        let ReadRequest {
            path,
            start,
            end,
            mut output,
            requester_name,
        } = read_request;
        match self.get_file_portion(path.clone(), start, end).await {
            Ok((file, size)) => {
                // TODO output.write success header
                // io::copy(&mut Box::into_pin(file), &mut output).await?;
                let mut buf: [u8; UPLOAD_BLOCK_SIZE] = [0; UPLOAD_BLOCK_SIZE];
                let mut file = Box::into_pin(file);
                let mut bytes_read: u64 = start.unwrap_or(0);
                let mut bytes_read_since_last_ui_update: u64 = 0;
                let mut last_ui_update = Instant::now();
                let mut speedometer = Speedometer::new(speedometer::WINDOW_SIZE);
                while let Ok(n) = file.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    bytes_read += n as u64;
                    bytes_read_since_last_ui_update += n as u64;
                    speedometer.entry(n);

                    let elapsed = last_ui_update.elapsed();
                    let should_emit = (bytes_read_since_last_ui_update
                        >= MIN_UPLOAD_UI_UPDATE_BYTES
                        && elapsed >= MIN_UPLOAD_UI_UPDATE_INTERVAL)
                        || elapsed >= MAX_UPLOAD_UI_UPDATE_INTERVAL;
                    if should_emit {
                        bytes_read_since_last_ui_update = 0;
                        last_ui_update = Instant::now();
                        if self
                            .event_broadcaster
                            .send(UiEvent::Uploaded(UploadInfo {
                                path: path.clone(),
                                bytes_read,
                                total_size: size,
                                speed: speedometer.measure(),
                                peer_name: requester_name.clone(),
                            }))
                            .is_err()
                        {
                            warn!("Ui response channel closed");
                        };
                    }
                    output.write_all(&buf[..n]).await?;
                    debug!("Uploaded {bytes_read} bytes of {size}");
                }
                if self
                    .event_broadcaster
                    .send(UiEvent::Uploaded(UploadInfo {
                        path: path.clone(),
                        bytes_read,
                        total_size: size,
                        speed: speedometer.measure(),
                        peer_name: requester_name,
                    }))
                    .is_err()
                {
                    warn!("Ui response channel closed");
                };
                output.finish()?;
                Ok(())
            }
            Err(rpc_error) => send_read_error(rpc_error, output).await,
        }
    }
    // This is what write_all does internally:
    // loop {
    //     if this.buf.is_empty() {
    //         return Poll::Ready(Ok(()));
    //     }
    //     let buf = this.buf;
    //     let n = ready!(this.stream.execute_poll(cx, |s| s.write(buf)))?;
    //     this.buf = &this.buf[n..];
    // }

    /// Get the potion of the file we want to upload
    async fn get_file_portion(
        &self,
        path: String,
        start: Option<u64>,
        end: Option<u64>,
    ) -> Result<(Box<dyn AsyncRead + Send>, u64), RpcError> {
        let start = start.unwrap_or(0);

        if let Some(end_offset) = end {
            if start > end_offset {
                return Err(RpcError::BadOffset);
            }
        }

        match self.shares.resolve_path(path) {
            Ok((resolved_path, size)) => match fs::File::open(resolved_path).await {
                Ok(mut file) => {
                    // Check file size matches what we have in the db
                    let metadata = file.metadata().await?;
                    let size_on_disk = metadata.len();
                    if size_on_disk != size {
                        error!("File size does not match that from db!");
                    }

                    // Seek to start point
                    file.seek(std::io::SeekFrom::Start(start))
                        .await
                        .map_err(|_| RpcError::BadOffset)?;

                    // If an endpoint is specified, only read until endpoint
                    match end {
                        Some(endpoint) => Ok((Box::new(file.take(endpoint - start)), size)),
                        None => Ok((Box::new(file), size)),
                    }
                }
                Err(_) => Err(RpcError::PathNotFound),
            },
            Err(_) => Err(RpcError::PathNotFound),
        }
    }
}

/// Respond with an error to a read query
// The issue here is that this is indistinguishable from actual output
// A solution might be to send a fixed length error code, and only parse it in the case that the
// stream ended prematurely
async fn send_read_error(error: RpcError, mut output: quinn::SendStream) -> Result<(), RpcError> {
    // TODO send serialised version of the error
    output.write_all(&[0]).await?;
    output.finish()?;
    Err(error)
}

/// Send an error response to an LS query
async fn send_ls_error(error: RpcError, mut output: quinn::SendStream) -> Result<(), RpcError> {
    // For LS query this should be length prefixed LsResponse::Err(LsResponseError)
    let response = LsResponse::Err(LsResponseError::InternalServer(error.to_string()));

    let buf = serialize(&response).map_err(|e| {
        error!("Cannot serialize query response {e:?}");
        RpcError::SerializeError
    })?;

    // Write the length prefix
    let length: u32 = buf
        .len()
        .try_into()
        .map_err(|_| RpcError::U64ConvertError)?;
    output.write_all(&length.to_be_bytes()).await?;

    output.write_all(&buf).await?;
    output.finish()?;
    Err(error)
}

/// Error to be sent as a wire message
#[derive(Error, Debug)]
pub enum RpcError {
    #[error("Db error")]
    DbError,
    #[error("Path not found")]
    PathNotFound,
    #[error("Bad offset")]
    BadOffset,
    #[error("Read error")]
    ReadError(#[from] std::io::Error),
    #[error("Write error")]
    WriteError(#[from] WriteError),
    #[error("serialize error")]
    SerializeError,
    #[error("Channel closed")]
    ChannelClosed,
    #[error("Cannot convert to u64")]
    U64ConvertError,
    #[error("Attempted to close an already closed stream")]
    ClosedStream(#[from] quinn::ClosedStream),
}

// fn create_error_stream(err: RpcError) -> Box<dyn Stream<Item = response::Response> + Send> {
//     let response = response::Response::Err(err as i32);
//     Box::new(stream::iter(vec![response]))
// }

#[cfg(test)]
mod upload_tests {
    use super::*;
    use crate::connections::quic::generate_certificate;
    use quinn::{ClientConfig, Connection, Endpoint, RecvStream, ServerConfig, TransportConfig};
    use tempfile::TempDir;
    use tokio::time::timeout;

    struct Fixture {
        _storage: TempDir,
        uploader: Uploader,
        server: Endpoint,
        client_config: ClientConfig,
        events: broadcast::Receiver<UiEvent>,
    }

    impl Fixture {
        async fn new() -> Self {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let storage = TempDir::new().unwrap();
            let share = storage.path().join("shared");
            fs::create_dir(&share).await.unwrap();
            let large = fs::File::create(share.join("large")).await.unwrap();
            large.set_len(1024 * 1024).await.unwrap();
            fs::write(share.join("small"), b"small").await.unwrap();
            let db = sled::Config::new().temporary(true).open().unwrap();
            let shares = Shares::new(db, vec![share.to_str().unwrap().into()])
                .await
                .unwrap();
            let (events_tx, events) = broadcast::channel(64);
            let uploader = Uploader {
                shares,
                event_broadcaster: events_tx,
            };

            let (cert, key) = generate_certificate().unwrap();
            let mut roots = rustls::RootCertStore::empty();
            roots.add(cert.clone()).unwrap();
            let mut client_config = ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();
            let mut transport = TransportConfig::default();
            transport.stream_receive_window(16_384u32.into());
            transport.receive_window(131_072u32.into());
            client_config.transport_config(Arc::new(transport));
            let mut server_config = ServerConfig::with_single_cert(vec![cert], key).unwrap();
            let mut transport = TransportConfig::default();
            transport.send_window(16_384);
            server_config.transport_config(Arc::new(transport));
            let server = Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
            Self {
                _storage: storage,
                uploader,
                server,
                client_config,
                events,
            }
        }

        async fn connect(&self) -> (Endpoint, Connection, Connection) {
            let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
            endpoint.set_default_client_config(self.client_config.clone());
            let client = endpoint
                .connect(self.server.local_addr().unwrap(), "peer")
                .unwrap();
            let (client, server) = tokio::join!(client, async {
                self.server.accept().await.unwrap().await.unwrap()
            });
            (endpoint, client.unwrap(), server)
        }

        fn spawn(&self) -> (UploadSender<ReadRequest>, tokio::task::JoinHandle<()>) {
            let uploader = self.uploader.clone();
            uploads::spawn(move |request| {
                let uploader = uploader.clone();
                async move { uploader.upload(request).await }
            })
        }
    }

    async fn request(
        client: &Connection,
        server: &Connection,
        peer: &str,
        path: &str,
    ) -> (ReadRequest, RecvStream) {
        let (mut send, recv) = client.open_bi().await.unwrap();
        send.write_all(b"request").await.unwrap();
        send.finish().unwrap();
        let (output, mut input) = server.accept_bi().await.unwrap();
        input.read_to_end(32).await.unwrap();
        (
            ReadRequest {
                path: path.into(),
                start: None,
                end: None,
                output,
                requester_name: peer.into(),
            },
            recv,
        )
    }

    #[tokio::test]
    async fn stalled_receiver_does_not_block_other_peer_and_stop_releases_its_slot() {
        timeout(Duration::from_secs(10), async {
            let mut fixture = Fixture::new().await;
            let (_endpoint_a, client_a, server_a) = fixture.connect().await;
            let (_endpoint_b, client_b, server_b) = fixture.connect().await;
            let (sender, handle) = fixture.spawn();
            let (large, mut large_recv) = request(&client_a, &server_a, "a", "shared/large").await;
            sender.send("a".into(), large).await.unwrap();
            // Observe that uploading began, then leave almost all data unread. The
            // file exceeds both receive credit and send buffering by a large margin.
            large_recv.read_exact(&mut [0; 1]).await.unwrap();
            let (next_a, mut next_a_recv) =
                request(&client_a, &server_a, "a", "shared/small").await;
            sender.send("a".into(), next_a).await.unwrap();
            let (mut next_b, mut next_b_recv) =
                request(&client_b, &server_b, "b", "shared/small").await;
            next_b.start = Some(1);
            next_b.end = Some(4);
            sender.send("b".into(), next_b).await.unwrap();
            assert_eq!(next_b_recv.read_to_end(16).await.unwrap(), b"mal");
            assert!(next_a_recv.read(&mut [0; 1]).now_or_never().is_none());
            let progress = loop {
                if let UiEvent::Uploaded(progress) = fixture.events.recv().await.unwrap() {
                    if progress.peer_name == "b" {
                        break progress;
                    }
                }
            };
            assert_eq!(progress.path, "shared/small");
            assert_eq!(progress.bytes_read, 4);
            assert_eq!(progress.total_size, 5);
            large_recv.stop(0u32.into()).unwrap();
            assert_eq!(next_a_recv.read_to_end(16).await.unwrap(), b"small");
            drop(sender);
            handle.await.unwrap();
        })
        .await
        .expect("uploads must progress despite a stalled receiver");
    }

    #[tokio::test]
    async fn queued_cancellation_is_skipped_before_opening_the_file() {
        timeout(Duration::from_secs(10), async {
            let fixture = Fixture::new().await;
            let (_endpoint, client, server) = fixture.connect().await;
            let (sender, handle) = fixture.spawn();
            let (large, mut large_recv) = request(&client, &server, "a", "shared/large").await;
            sender.send("a".into(), large).await.unwrap();
            large_recv.read_exact(&mut [0; 1]).await.unwrap();
            let (canceled, mut canceled_recv) =
                request(&client, &server, "a", "shared/missing").await;
            let stopped = canceled.output.stopped();
            sender.send("a".into(), canceled).await.unwrap();
            canceled_recv.stop(0u32.into()).unwrap();
            stopped.await.unwrap();
            let (next, mut next_recv) = request(&client, &server, "a", "shared/small").await;
            sender.send("a".into(), next).await.unwrap();
            large_recv.stop(0u32.into()).unwrap();
            assert_eq!(next_recv.read_to_end(16).await.unwrap(), b"small");
            drop(sender);
            handle.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn disconnected_upload_releases_peer_for_replacement_connection() {
        timeout(Duration::from_secs(10), async {
            let fixture = Fixture::new().await;
            let (_old_endpoint, old_client, old_server) = fixture.connect().await;
            let (_new_endpoint, new_client, new_server) = fixture.connect().await;
            let (sender, handle) = fixture.spawn();
            let (large, mut recv) = request(&old_client, &old_server, "a", "shared/large").await;
            sender.send("a".into(), large).await.unwrap();
            recv.read_exact(&mut [0; 1]).await.unwrap();
            let (next, mut next_recv) =
                request(&new_client, &new_server, "a", "shared/small").await;
            sender.send("a".into(), next).await.unwrap();
            old_client.close(0u32.into(), b"test disconnect");
            assert_eq!(next_recv.read_to_end(16).await.unwrap(), b"small");
            drop(sender);
            handle.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn uploader_returns_without_opening_a_canceled_request() {
        timeout(Duration::from_secs(10), async {
            let fixture = Fixture::new().await;
            let (_endpoint, client, server) = fixture.connect().await;
            let (canceled, mut recv) = request(&client, &server, "a", "shared/missing").await;
            let stopped = canceled.output.stopped();
            recv.stop(0u32.into()).unwrap();
            stopped.await.unwrap();
            // Opening this nonexistent file would return PathNotFound instead.
            fixture.uploader.upload(canceled).await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn canceled_read_does_not_wait_for_queue_capacity() {
        timeout(Duration::from_secs(10), async {
            let fixture = Fixture::new().await;
            let (_endpoint, client, server) = fixture.connect().await;
            let (sender, handle) = fixture.spawn();
            let (large, mut large_recv) = request(&client, &server, "a", "shared/large").await;
            sender.send("a".into(), large).await.unwrap();
            large_recv.read_exact(&mut [0; 1]).await.unwrap();
            let mut queued_receivers = Vec::new();
            for _ in 0..8 {
                let (next, recv) = request(&client, &server, "a", "shared/small").await;
                sender.send("a".into(), next).await.unwrap();
                queued_receivers.push(recv);
            }
            let db = sled::Config::new().temporary(true).open().unwrap();
            let rpc = Rpc {
                shares: fixture.uploader.shares.clone(),
                upload_tx: sender.clone(),
                peer_announce_tx: tokio::sync::mpsc::channel(1).0,
                peers: Default::default(),
                known_peers: KnownPeers::new(db.open_tree("peers").unwrap()),
            };
            let (next, mut recv) = request(&client, &server, "a", "shared/small").await;
            let mut admission = Box::pin(rpc.read(
                next.path,
                next.start,
                next.end,
                next.output,
                next.requester_name,
            ));
            assert!(admission.as_mut().now_or_never().is_none());
            recv.stop(0u32.into()).unwrap();
            admission.await.unwrap();
            // The active receiver remains stalled throughout canceled admission.
            large_recv.stop(0u32.into()).unwrap();
            drop(queued_receivers);
            drop(rpc);
            drop(sender);
            handle.await.unwrap();
        })
        .await
        .unwrap();
    }
}
