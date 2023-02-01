use std::{collections::HashMap, net::SocketAddr, path::Path, sync::Arc};

use crate::{
    connect::make_server_endpoint,
    rpc::Rpc,
    shares::Shares,
    ui_messages::{Command, UiClientMessage, UiResponse, UiServerError, UiServerMessage},
    wire_messages::{LsResponse, Request},
};
use bincode::{deserialize, serialize};
use log::{debug, warn};
use quinn::{Connection, Endpoint, RecvStream};
use thiserror::Error;
use tokio::{
    select,
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
};

pub struct Hdp {
    peers: HashMap<String, Connection>,
    rpc: Arc<Rpc>,
    endpoint: Endpoint,
    pub command_tx: UnboundedSender<UiClientMessage>,
    command_rx: UnboundedReceiver<UiClientMessage>,
    response_tx: UnboundedSender<UiServerMessage>,
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
        let (endpoint, _cert) = make_server_endpoint("127.0.0.1:0".parse()?)?;
        Ok((
            Self {
                peers: Default::default(),
                rpc: Arc::new(Rpc::new(shares)),
                endpoint,
                command_tx,
                command_rx,
                response_tx,
                // public_key,
                // name: to_hex_string(public_key),
            },
            response_rx,
        ))
    }

    pub async fn run(&mut self) {
        loop {
            select! {
                Some(incoming_conn) = self.endpoint.accept() => {
                    self.handle_connection(incoming_conn).await;
                }
                Some(command) = self.command_rx.recv() => {
                    if let Err(err) = self.handle_command(command).await {
                        warn!("Closing connection {err}");
                        break;
                    };
                }
            }
        }
    }

    /// Open a request stream and write a request to the given peer
    async fn request(&self, request: Request, name: &str) -> Result<RecvStream, RequestError> {
        let connection = self.peers.get(name).ok_or(RequestError::PeerNotFound)?;
        let (mut send, recv) = connection.open_bi().await?;
        let buf = serialize(&request).map_err(|_| RequestError::SerializationError)?;

        send.write_all(&buf).await?;
        send.finish().await?;
        Ok(recv)
    }

    /// Handle an incoming connection from a remote peer
    async fn handle_connection(&mut self, incoming_conn: quinn::Connecting) {
        match incoming_conn.await {
            Ok(conn) => {
                debug!("connection accepted {}", conn.remote_address());

                if let Some(i) = conn.peer_identity() {
                    debug!("Client cert {:?}", i.downcast::<Vec<rustls::Certificate>>());
                }
                self.peers
                    .insert(conn.remote_address().to_string(), conn.clone());

                let rpc = self.rpc.clone();
                tokio::spawn(async move {
                    while let Ok((send, recv)) = conn.accept_bi().await {
                        match recv.read_to_end(1024).await {
                            Ok(buf) => {
                                // request::decode(req);
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
                                        warn!("Cannot decode message");
                                    }
                                }
                            }
                            Err(err) => {
                                warn!("Cannot read from incoming connection {:?}", err);
                                break;
                            }
                        };
                    }
                });
            }
            Err(err) => {
                warn!("Incoming connection failed {:?}", err);
            }
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
                self.endpoint.wait_idle().await;
                return Err(HandleUiCommandError::ConnectionClosed);
            }
            Command::Connect(addr) => {
                let response = self.connect_to_peer(addr).await;
                if self
                    .response_tx
                    .send(UiServerMessage { id, response })
                    .is_err()
                {
                    return Err(HandleUiCommandError::ChannelClosed);
                }
            }
            Command::Request(req, name) => {
                let req_clone = req.clone();
                match self.request(req, &name).await {
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
                                        let mut msg_buf =
                                            vec![Default::default(); length.try_into().unwrap()];
                                        recv.read_exact(&mut msg_buf).await.unwrap();
                                        let ls_response: LsResponse =
                                            deserialize(&msg_buf).unwrap();

                                        if response_tx
                                            .send(UiServerMessage {
                                                id,
                                                response: Ok(UiResponse::Ls(ls_response)),
                                            })
                                            .is_err()
                                        {
                                            warn!("Response channel closed");
                                            break;
                                        }
                                    }

                                    // Terminate with an endresponse or an error
                                    if response_tx
                                        .send(UiServerMessage {
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
                                            .send(UiServerMessage {
                                                id,
                                                response: Ok(UiResponse::Read(buf[..n].to_vec())),
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
                        if self
                            .response_tx
                            .send(UiServerMessage {
                                id,
                                response: Err(UiServerError::RequestError(err)),
                            })
                            .is_err()
                        {
                            return Err(HandleUiCommandError::ChannelClosed);
                        }
                    }
                };
            }
        };
        Ok(())
    }

    async fn connect_to_peer(&mut self, addr: SocketAddr) -> Result<UiResponse, UiServerError> {
        let connection = self
            .endpoint
            .connect(addr, "localhost")
            .map_err(|_| UiServerError::ConnectionError)?
            .await
            .map_err(|_| UiServerError::ConnectionError)?;

        if let Some(i) = connection.peer_identity() {
            debug!(
                "[client] connected: addr={:#?}",
                i.downcast::<Vec<rustls::Certificate>>()
            );
        };

        self.peers
            .insert(connection.remote_address().to_string(), connection);
        Ok(UiResponse::Connect)
    }
}

#[derive(Error, Debug)]
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

#[derive(Error, Debug)]
pub enum HandleUiCommandError {
    #[error("User closed connection")]
    ConnectionClosed,
    #[error("Channel closed - could not send response")]
    ChannelClosed,
}

#[cfg(test)]
mod tests {
    use crate::wire_messages::Entry;

    use super::*;
    use tempfile::TempDir;

    async fn setup_peer(share_dirs: Vec<&str>) -> (Hdp, UnboundedReceiver<UiServerMessage>) {
        let storage = TempDir::new().unwrap();
        Hdp::new(storage, share_dirs).await.unwrap()
    }

    #[tokio::test]
    async fn test_read() -> Result<(), Box<dyn std::error::Error>> {
        env_logger::init();
        let (mut alice, _alice_rx) = setup_peer(vec!["tests/test-data"]).await;
        let alice_addr = alice.endpoint.local_addr().unwrap();

        let alice_command_tx = alice.command_tx.clone();
        tokio::spawn(async move {
            alice.run().await;
        });

        let (mut bob, mut bob_rx) = setup_peer(vec![]).await;
        let bob_command_tx = bob.command_tx.clone();
        tokio::spawn(async move {
            bob.run().await;
        });

        // Connect to alice
        bob_command_tx
            .send(UiClientMessage {
                id: 0,
                command: Command::Connect(alice_addr),
            })
            .unwrap();

        let _res = bob_rx.recv().await.unwrap();

        // Do a read request
        let req = Request::Read {
            path: "test-data/somefile".to_string(),
            start: None,
            end: None,
        };
        bob_command_tx
            .send(UiClientMessage {
                id: 1,
                command: Command::Request(req, alice_addr.to_string()),
            })
            .unwrap();

        let res = bob_rx.recv().await.unwrap();
        assert_eq!(UiResponse::Read(b"boop\n".to_vec()), res.response.unwrap());

        // Do an Ls query
        let req = Request::Ls {
            path: None,
            searchterm: None,
            recursive: true,
        };
        bob_command_tx
            .send(UiClientMessage {
                id: 1,
                command: Command::Request(req, alice_addr.to_string()),
            })
            .unwrap();

        let mut entries = Vec::new();
        while let UiResponse::Ls(LsResponse::Success(some_entries)) =
            bob_rx.recv().await.unwrap().response.unwrap()
        {
            for entry in some_entries {
                entries.push(entry);
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
