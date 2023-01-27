use crate::protocol::{Event, Protocol};
use crate::rpc::Rpc;
use crate::shares::{CreateSharesError, Shares};
use crate::{
    messages::{
        request,
        response::{self, ls::Entry, Success},
    },
    protocol::PeerConnectionError,
};
use async_channel::{Receiver, SendError, Sender};
use async_std::task::{self, JoinHandle};
use futures::io::{AsyncRead, AsyncWrite};
use futures::{select, StreamExt};
use log::{info, warn};
use quinn::Connection;
use rand::Rng;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct OutGoingPeerRequest {
    pub message: request::Msg,
    pub response_tx: Sender<IncomingPeerResponse>,
}

#[derive(Debug)]
pub struct IncomingPeerResponse {
    pub message: response::Response,
    pub public_key: [u8; 32],
}

pub struct IncomingPeerRequest {
    pub message: request::Msg,
    pub response_tx: quinn::SendStream,
}

// pub struct OutgoingPeerResponse {
//     pub id: u32,
//     pub message: response::Response,
// }

pub trait AsyncReadAndWrite: AsyncWrite + AsyncRead + Send + Unpin + 'static {}

/// Main loop
pub struct Run {
    peers: HashMap<String, Sender<OutGoingPeerRequest>>,
    pub rpc: Rpc,
    requests_to_us_tx: Sender<IncomingPeerRequest>,
    requests_to_us_rx: Receiver<IncomingPeerRequest>,
    pub public_key: [u8; 32],
    pub name: String,
}

impl Run {
    pub async fn new(storage: impl AsRef<Path>) -> Result<Self, CreateSharesError> {
        // TODO this will be replaced by a noise public key
        let mut rng = rand::thread_rng();
        let public_key = rng.gen();
        let shares = Shares::new(storage).await?;
        let (requests_tx, requests_rx) = async_channel::unbounded();
        Ok(Self {
            peers: Default::default(),
            rpc: Rpc::new(shares),
            requests_to_us_tx: requests_tx,
            requests_to_us_rx: requests_rx,
            public_key,
            name: to_hex_string(public_key),
        })
    }

    /// Loop for IO for a connected peer
    pub async fn handle_peer(
        &mut self,
        connection: Connection,
        is_initiator: bool,
    ) -> JoinHandle<()> {
        // TODO handle the case that the handshake fails
        // let mut peer_connection =
        //     Protocol::with_handshake(Box::into_pin(peer_stream), self.public_key, is_initiator)
        //         .await
        //         .unwrap();
        // info!("Remote pk {:?}", peer_connection.remote_pk.unwrap());

        let requests_to_us_tx = self.requests_to_us_tx.clone();

        // let (response_tx, response_rx): (
        //     async_channel::Sender<OutgoingPeerResponse>,
        //     async_channel::Receiver<OutgoingPeerResponse>,
        // ) = async_channel::unbounded();
        //
        let (requests_from_us_tx, requests_from_us_rx): (
            async_channel::Sender<OutGoingPeerRequest>,
            async_channel::Receiver<OutGoingPeerRequest>,
        ) = async_channel::unbounded();
        // self.peers.insert(
        //     to_hex_string(peer_connection.remote_pk.unwrap()),
        //     requests_from_us_tx,
        // );
        task::spawn(async move {
            let mut response_rx = response_rx.fuse();
            let mut requests_from_us_rx = requests_from_us_rx.fuse();

            // here we need a loop for incoming requests
            while let Ok((mut send, recv)) = connection.accept_bi().await {
                // recieve a request
                // send it to rpc
            }

            // TODO if the connection closes, this loop should end, and the peer should be removed
            // from self.peers
            loop {
                info!("looping {}", is_initiator);
                select! {
                    next_event = peer_connection.next() => {
                        info!("next stream");
                        match next_event {
                            Some(Ok(Event::Request(message, id))) => {
                                info!("got request {:?} {}", message, id);
                                match requests_to_us_tx
                                    .send(IncomingPeerRequest {
                                        message,
                                        id,
                                        response_tx: response_tx.clone(),
                                        // peer_id: "TODO".to_string(),
                                    })
                                    .await {
                                        Ok(()) => {},
                                        Err(e) => {
                                            warn!("Error {}",e);
                                        }
                                    }
                            },
                            Some(Err(PeerConnectionError::BadMessageError)) => {
                                warn!("Bad message from peer");
                            },
                            Some(Err(PeerConnectionError::ConnectionError)) => {
                                break;
                            },
                            _ => {}
                        }
                    },
                    next_res = response_rx.next() => {
                        info!("Sending outgoing response");
                        match next_res {
                            Some(next_res) => {
                                // TODO this will panic if the channel is closed
                                peer_connection
                                    .respond(next_res.message, next_res.id)
                                    .await
                                    .unwrap();
                                },
                            None => {
                                warn!("got none on response channel");
                            },

                        }
                    },
                    next_req = requests_from_us_rx.next() => {
                        info!("Sending outgoing req {:?} {}", next_req, is_initiator);
                        match next_req {
                            Some(next_req) => {
                                peer_connection.request(next_req).await.unwrap();
                            },
                            None => {
                                warn!("got none on own requests channel!");
                            },
                        }
                    },
                }
            }
            // remove peer from self.peers
        })
    }

    /// Make a query to connected peers
    // TODO this will take a channel tx and send responses over it
    pub async fn ls(
        &self,
        path: Option<String>,
        searchterm: Option<String>,
        recursive: bool,
    ) -> Vec<Entry> {
        let path_buf = match path {
            Some(path_string) => match path_string.as_str() {
                "" => None,
                ps => Some(PathBuf::from(ps)),
            },
            None => None,
        };
        let path_given = path_buf.is_some();

        let (response_tx, mut response_rx) = async_channel::unbounded();
        match path_buf {
            Some(puf) => {
                let peer_name = puf.iter().next().unwrap().to_str().unwrap();
                let remaining_path = puf
                    .strip_prefix(peer_name)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string();

                let peer = self.peers.get(peer_name).unwrap();
                peer.send(OutGoingPeerRequest {
                    response_tx,
                    message: request::Msg::Ls(request::Ls {
                        path: Some(remaining_path),
                        searchterm,
                        recursive,
                    }),
                })
                .await
                .unwrap();
            }
            None => {
                for (_name, peer) in self.peers.iter() {
                    peer.send(OutGoingPeerRequest {
                        response_tx: response_tx.clone(),
                        message: request::Msg::Ls(request::Ls {
                            path: None,
                            searchterm: searchterm.clone(),
                            recursive,
                        }),
                    })
                    .await
                    .unwrap();
                }
            }
        };

        let mut ls_entries = Vec::new();
        while let Some(res) = response_rx.next().await {
            // TODO if we get an error, return it
            if let IncomingPeerResponse {
                public_key,
                message:
                    response::Response::Success(Success {
                        msg: Some(response::success::Msg::Ls(response::Ls { entries })),
                    }),
            } = res
            {
                for entry in entries.iter() {
                    // Only output the top level entry if no path is given and not recursive
                    if path_given || recursive || entry.name.is_empty() {
                        ls_entries.push(Entry {
                            name: format!("{}/{}", to_hex_string(public_key), entry.name.clone()),
                            size: entry.size,
                            is_dir: entry.is_dir,
                        });
                    };
                }
                // TODO here we would send ls_entries over a channel
            };
        }
        ls_entries
    }

    /// Read a file from a particular peer, giving "peername/pathtofile"
    // TODO this will take a channel tx and send the responses over it
    pub async fn read_file(&self, path: &str) -> String {
        let path_buf = PathBuf::from(path);
        let peer_name = path_buf.iter().next().unwrap().to_str().unwrap();
        let remaining_path = path_buf.strip_prefix(peer_name).unwrap();
        let peer = self.peers.get(peer_name).unwrap(); // .ok_or(error);

        let mut output = Vec::new();
        let (response_tx, mut response_rx) = async_channel::unbounded();
        peer.send(OutGoingPeerRequest {
            response_tx,
            message: request::Msg::Read(request::Read {
                start: None,
                end: None,
                path: remaining_path.to_str().unwrap().to_string(),
            }),
        })
        .await
        .unwrap();

        while let Some(res) = response_rx.next().await {
            // TODO if we get an error, return it
            match res {
                IncomingPeerResponse {
                    public_key: _,
                    message:
                        response::Response::Success(Success {
                            msg: Some(response::success::Msg::Read(response::Read { mut data })),
                        }),
                } => {
                    output.append(&mut data);
                }
                thing => {
                    warn!("thing {:?}", thing);
                }
            }
        }
        String::from_utf8(output).unwrap()
    }

    /// Run the internal RPC loop
    pub async fn run(mut self) -> Result<(), SendError<OutgoingPeerResponse>> {
        self.rpc.run(self.requests_to_us_rx).await
    }
}

// TODO this will be replaced by 'key to animal', deriving a silly name from peer public keys
fn to_hex_string(bytes: [u8; 32]) -> String {
    let strs: Vec<String> = bytes.iter().take(2).map(|b| format!("{:02x}", b)).collect();
    strs.join("")
}
