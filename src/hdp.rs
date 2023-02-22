use crate::{
    connect::make_server_endpoint,
    mdns::{mdns_server, MdnsPeerInfo},
    rpc::Rpc,
    shares::Shares,
    ui_messages::{Command, UiClientMessage, UiResponse, UiServerError, UiServerMessage},
    wire_messages::{LsResponse, Request},
};
use bincode::{deserialize, serialize};
use local_ip_address::local_ip;
use log::{debug, info, warn};
use quinn::{Connection, Endpoint, RecvStream};
use std::{collections::HashMap, net::SocketAddr, path::Path, sync::Arc};
use thiserror::Error;
use tokio::{
    select,
    sync::{
        mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
};

const MAX_REQUEST_SIZE: usize = 1024;

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
    /// Channel for discovered mdns peers
    mdns_peers_rx: UnboundedReceiver<MdnsPeerInfo>,
}

impl Hdp {
    pub async fn new(
        storage: impl AsRef<Path>,
        sharedirs: Vec<&str>,
    ) -> anyhow::Result<(Self, UnboundedReceiver<UiServerMessage>)> {
        // TODO this will be replaced by a noise public key
        // let mut rng = rand::thread_rng();
        // let public_key = rng.gen();

        let (command_tx, command_rx) = unbounded_channel();
        let (response_tx, response_rx) = unbounded_channel();
        let shares = Shares::new(storage, sharedirs).await?;

        let my_local_ip = local_ip()?;

        let (endpoint, _cert) = make_server_endpoint(SocketAddr::new(my_local_ip, 0))?;

        let addr = endpoint.local_addr()?;
        let topic = "boop".to_string(); // TODO
        let mdns_peers_rx = mdns_server(&addr.to_string(), addr, topic).await?;
        Ok((
            Self {
                peers: Default::default(),
                rpc: Arc::new(Rpc::new(shares)),
                endpoint,
                command_tx,
                command_rx,
                response_tx,
                mdns_peers_rx,
                // public_key,
                // name: to_hex_string(public_key),
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
                Some(mdns_peer_info) = self.mdns_peers_rx.recv() => {
                    debug!("Found mdns peer {}", mdns_peer_info.addr);
                    if self.connect_to_peer(mdns_peer_info.addr).await.is_err() {
                        warn!("Cannot connect to mdns peer");
                    };
                }
            }
        }
    }

    // Handle a QUIC connection from/to another peer
    async fn handle_connection(&mut self, conn: quinn::Connection, incoming: bool) {
        let direction = if incoming { "incoming" } else { "outgoing" };

        let peers_clone = self.peers.clone();
        let mut peers = self.peers.lock().await;
        info!("[{}] before connected to {} peers", direction, peers.len());
        peers.insert(conn.remote_address().to_string(), conn.clone());
        info!("[{}] connected to {} peers", direction, peers.len());

        let rpc = self.rpc.clone();
        tokio::spawn(async move {
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
                        warn!("Error when accepting QUIC stream {:?}", error);
                        break;
                    }
                }
            }
            let mut peers = peers_clone.lock().await;
            match peers.remove(&conn.remote_address().to_string()) {
                Some(_) => {
                    debug!("Connection closed - removed peer");
                    // TODO here we should send a peer disconnected event to the UI
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
                debug!("connection accepted {}", conn.remote_address());

                if let Some(i) = conn.handshake_data() {
                    let d = i
                        .downcast::<quinn::crypto::rustls::HandshakeData>()
                        .unwrap();
                    debug!("Server name {:?}", d.server_name);
                }

                if let Some(i) = conn.peer_identity() {
                    debug!("Client cert {:?}", i.downcast::<Vec<rustls::Certificate>>());
                }

                self.handle_connection(conn, true).await;
            }
            Err(err) => {
                warn!("Incoming QUIC connection failed {:?}", err);
            }
        }
    }

    /// Initiate a Quic connection to a remote peer
    async fn connect_to_peer(&mut self, addr: SocketAddr) -> Result<UiResponse, UiServerError> {
        {
            let peers = self.peers.lock().await;
            if peers.contains_key(&addr.to_string()) {
                warn!("Connecting to existing peer!");
            }
        }
        let connection = self
            .endpoint
            .connect(addr, "ssss") // TODO
            .map_err(|_| UiServerError::ConnectionError)?
            .await
            .map_err(|_| UiServerError::ConnectionError)?;

        // if let Some(i) = connection.peer_identity() {
        //     debug!(
        //         "[client] connected: addr={:#?}",
        //         i.downcast::<Vec<rustls::Certificate>>()
        //     );
        // };

        self.handle_connection(connection, false).await;
        Ok(UiResponse::Connect)
    }

    /// Handle a command from the UI
    async fn handle_command(
        &mut self,
        ui_client_message: UiClientMessage,
    ) -> Result<(), HandleUiCommandError> {
        let id = ui_client_message.id;
        match ui_client_message.command {
            Command::Close => {
                self.endpoint.wait_idle().await;
                return Err(HandleUiCommandError::ConnectionClosed);
            }
            Command::Connect(addr) => {
                let response = self.connect_to_peer(addr).await;
                if self
                    .response_tx
                    .send(UiServerMessage::Response { id, response })
                    .is_err()
                {
                    return Err(HandleUiCommandError::ChannelClosed);
                }
            }
            Command::Request(req, name) => {
                let requests = self.expand_request(req, name).await;
                for (request, peer_name) in requests {
                    info!("Sending request to {}", peer_name);
                    let req_clone = request.clone();
                    let peer_name_clone = peer_name.clone();
                    match self.request(request, &peer_name).await {
                        Ok(mut recv) => {
                            let response_tx = self.response_tx.clone();
                            tokio::spawn(async move {
                                match req_clone {
                                    Request::Ls {
                                        path: _,
                                        searchterm: _,
                                        recursive: _,
                                    } => {
                                        // Read the length prefix
                                        // TODO this should be a varint
                                        let mut length_buf: [u8; 8] = [0; 8];
                                        while let Ok(()) = recv.read_exact(&mut length_buf).await {
                                            let length: u64 = u64::from_le_bytes(length_buf);
                                            debug!("Read prefix {length}");

                                            // Read a message
                                            let mut msg_buf = vec![
                                                Default::default();
                                                length.try_into().unwrap()
                                            ];
                                            match recv.read_exact(&mut msg_buf).await {
                                                Ok(()) => {
                                                    let ls_response: LsResponse =
                                                        deserialize(&msg_buf).unwrap();

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
                                                Err(_) => {
                                                    warn!("Bad prefix / read error");
                                                    break;
                                                }
                                            }
                                        }

                                        // Terminate with an endresponse
                                        // TODO if there was more then one peer we need to only
                                        // send this if we are the last one
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
                                    Request::Read {
                                        path: _,
                                        start: _,
                                        end: _,
                                    } => {
                                        // TODO write to file
                                        let mut buf: [u8; 1024] = [0; 1024];
                                        while let Ok(Some(n)) = recv.read(&mut buf).await {
                                            debug!("Read {} bytes", n);
                                            if response_tx
                                                .send(UiServerMessage::Response {
                                                    id,
                                                    response: Ok(UiResponse::Read(
                                                        buf[..n].to_vec(),
                                                    )),
                                                })
                                                .is_err()
                                            {
                                                warn!("Response channel closed");
                                                break;
                                            };
                                        }
                                        // Terminate with an endresponse or an error
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
