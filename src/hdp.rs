use std::{collections::HashMap, path::Path};

use crate::{
    messages::Request,
    rpc::Rpc,
    shares::{CreateSharesError, Shares},
};
use bincode::{deserialize, serialize};
use log::debug;
use quinn::Connection;
// use tokio::sync::mpsc::unbounded_channel;

pub struct Hdp {
    peers: HashMap<String, Connection>,
    rpc: Rpc,
}

impl Hdp {
    pub async fn new(storage: impl AsRef<Path>) -> Result<Self, CreateSharesError> {
        // TODO this will be replaced by a noise public key
        // let mut rng = rand::thread_rng();
        // let public_key = rng.gen();

        let shares = Shares::new(storage).await?;
        Ok(Self {
            peers: Default::default(),
            rpc: Rpc::new(shares),
            // public_key,
            // name: to_hex_string(public_key),
        })
    }

    pub async fn handle_conn(&mut self, connection: Connection) {
        self.peers.insert("boop".to_string(), connection.clone());
        if let Ok((send, recv)) = connection.accept_bi().await {
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
                            self.rpc.ls(path, searchterm, recursive, send).await;
                        }
                        Request::Read { path, start, end } => {
                            self.rpc.read(path, start, end, send).await.unwrap();
                        }
                    }
                }
                Err(_) => {
                    println!("cannot decode");
                }
            }
        }
    }

    pub fn add_connection(&mut self, connection: Connection) {
        self.peers.insert("boop".to_string(), connection.clone());
    }

    pub async fn request(&self, request: Request) {
        let connection = self.peers.get("boop").unwrap();
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
    use crate::connect::make_server_endpoint;
    use log::info;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_read() -> Result<(), Box<dyn std::error::Error>> {
        env_logger::init();
        let storage_a = TempDir::new().unwrap();
        let mut peer_a = Hdp::new(storage_a).await.unwrap();
        peer_a.rpc.shares.scan("tests_/test-data").await.unwrap();
        let server_addr = "127.0.0.1:5000".parse().unwrap();
        let (endpoint, _server_cert) = make_server_endpoint(server_addr)?;

        tokio::spawn(async move {
            if let Some(incoming_conn) = endpoint.accept().await {
                let conn = incoming_conn.await.unwrap();
                info!(
                    "[server] connection accepted: addr={}",
                    conn.remote_address()
                );

                if let Some(i) = conn.peer_identity() {
                    info!(
                        "[peer] connected: addr={:#?}",
                        i.downcast::<Vec<rustls::Certificate>>()
                    );
                }
                peer_a.handle_conn(conn).await;
            }
        });

        let (endpoint, _server_cert) = make_server_endpoint("127.0.0.1:5001".parse().unwrap())?;

        let storage_b = TempDir::new().unwrap();
        let mut peer_b = Hdp::new(storage_b).await.unwrap();

        let client_connection = endpoint
            .connect(server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();

        if let Some(i) = client_connection.peer_identity() {
            println!(
                "[client] connected: addr={:#?}",
                i.downcast::<Vec<rustls::Certificate>>()
            );
        }

        peer_b.add_connection(client_connection);

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
        peer_b.request(req).await;

        // Make sure the server has a chance to clean up
        endpoint.wait_idle().await;
        Ok(())
    }
}
