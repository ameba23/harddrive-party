use std::{collections::HashMap, net::SocketAddr, path::Path, sync::Arc};

use crate::{
    connect::make_server_endpoint,
    messages::Request,
    rpc::Rpc,
    shares::{CreateSharesError, Shares},
};
use bincode::{deserialize, serialize};
use log::{debug, warn};
use quinn::{Connection, Endpoint};
use tokio::{
    select,
    sync::mpsc::{error::SendError, unbounded_channel, UnboundedReceiver, UnboundedSender},
};

pub struct CommandMessage {
    id: u32,
    command: Command,
}

#[derive(Debug)]
pub enum Command {
    Connect(SocketAddr),
    Request(Request, String),
    Close,
}

pub enum UiResponse {}

pub enum UiError {}

pub struct ResponseMessage {
    id: u32,
    response: Result<UiResponse, UiError>,
}

pub struct Hdp {
    peers: HashMap<String, Connection>,
    rpc: Arc<Rpc>,
    endpoint: Endpoint,
    pub command_tx: UnboundedSender<Command>,
    command_rx: UnboundedReceiver<Command>,
    response_tx: UnboundedSender<ResponseMessage>,
    pub response_rx: UnboundedReceiver<ResponseMessage>,
}

impl Hdp {
    pub async fn new(
        storage: impl AsRef<Path>,
        sharedirs: Vec<&str>,
    ) -> Result<Self, CreateSharesError> {
        // TODO this will be replaced by a noise public key
        // let mut rng = rand::thread_rng();
        // let public_key = rng.gen();

        let (command_tx, command_rx) = unbounded_channel();
        let (response_tx, response_rx) = unbounded_channel();
        let shares = Shares::new(storage, sharedirs).await?;
        let (endpoint, _cert) = make_server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        Ok(Self {
            peers: Default::default(),
            rpc: Arc::new(Rpc::new(shares)),
            endpoint,
            command_tx,
            command_rx,
            response_tx,
            response_rx,
            // public_key,
            // name: to_hex_string(public_key),
        })
    }

    pub async fn run(&mut self) {
        loop {
            println!("i am here");
            select! {
                        Some(incoming_conn) = self.endpoint.accept() => {
                            let conn = incoming_conn.await.unwrap();
                            debug!("connection accepted {}", conn.remote_address());

                            if let Some(i) = conn.peer_identity() {
                                debug!("Client cert {:?}", i.downcast::<Vec<rustls::Certificate>>());
                            }
                            self.peers
                                .insert(conn.remote_address().to_string(), conn.clone());

                            let rpc = self.rpc.clone();
                            tokio::spawn(async move {
                                while let Ok((send, recv)) = conn.accept_bi().await {
                                    let buf = recv.read_to_end(1024).await.unwrap();
                                    // request::decode(req);
                                    let request: Result<Request, Box<bincode::ErrorKind>> = deserialize(&buf);
                                    match request {
                                        Ok(req) => {
                                            println!("{:?}", req);
                                            match req {
                                                Request::Ls {
                                                    path,
                                                    searchterm,
                                                    recursive,
                                                } => {
                                                        rpc.ls(path, searchterm, recursive, send).await;
                                                    }
                                                Request::Read { path, start, end } => {
                                                    rpc.read(path, start, end, send).await.unwrap();
                                                }
                                            }
                                        }
                                        Err(_) => {
                                            warn!("Cannot decode message");
                                        }
                                    }
                                }
                            });
                        }
                Some(command) = self.command_rx.recv() => {
                    match command {
                        Command::Close => {
                            self.endpoint.wait_idle().await;
                            break;
                        }
                        Command::Connect(addr) => {
                            let connection = self.endpoint
                                .connect(addr, "localhost")
                                .unwrap()
                                .await
                                .unwrap();

                            if let Some(i) = connection.peer_identity() {
                                debug!(
                                    "[client] connected: addr={:#?}",
                                    i.downcast::<Vec<rustls::Certificate>>()
                                );
                            }

                            self.peers
                                .insert(connection.remote_address().to_string(), connection);
                        }
                        Command::Request(req, name) => {
                            self.request(req, &name).await;
                        }
                    }
                }
            }
        }
    }

    // pub fn close(&self) -> Result<(), SendError<Command>> {
    //     self.command_tx.send(Command::Close)?;
    //     Ok(())
    // }

    pub async fn request(&self, request: Request, name: &str) {
        let connection = self.peers.get(name).unwrap();
        if let Ok((mut send, recv)) = connection.open_bi().await {
            let buf = serialize(&request).unwrap();

            send.write_all(&buf).await.unwrap();
            send.finish().await.unwrap();

            let msg = recv.read_to_end(1024).await.unwrap();
            println!("{:?}", msg);
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::info;
    use tempfile::TempDir;

    async fn setup_peer(share_dirs: Vec<&str>) -> Hdp {
        let storage = TempDir::new().unwrap();
        Hdp::new(storage, share_dirs).await.unwrap()
    }

    #[tokio::test]
    async fn test_read() -> Result<(), Box<dyn std::error::Error>> {
        env_logger::init();
        let mut alice = setup_peer(vec!["tests/test-data"]).await;
        let alice_addr = alice.endpoint.local_addr().unwrap();

        let alice_command_tx = alice.command_tx.clone();
        tokio::spawn(async move {
            alice.run().await;
        });

        let mut bob = setup_peer(vec![]).await;
        let bob_command_tx = bob.command_tx.clone();
        tokio::spawn(async move {
            bob.run().await;
        });

        bob_command_tx.send(Command::Connect(alice_addr)).unwrap();
        // let req = Request::Read {
        //     path: "test-data/somefile".to_string(),
        //     start: None,
        //     end: None,
        // };
        let req = Request::Ls {
            path: None,
            searchterm: None,
            recursive: true,
        };
        bob_command_tx
            .send(Command::Request(req, alice_addr.to_string()))
            .unwrap();
        // alice.close().unwrap();
        // bob_endpoint.wait_idle().await;
        alice_command_tx.send(Command::Close).unwrap();
        Ok(())
    }
}
