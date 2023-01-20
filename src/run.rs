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
use async_channel::{Receiver, Sender};
use async_std::task::{self, JoinHandle};
use futures::io::{AsyncRead, AsyncWrite};
use futures::{select, StreamExt};
use log::{info, warn};
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
    pub id: u32,
    pub response_tx: Sender<OutgoingPeerResponse>,
}

pub struct OutgoingPeerResponse {
    pub id: u32,
    pub message: response::Response,
}

pub trait AsyncReadAndWrite: AsyncWrite + AsyncRead + Send + Unpin + 'static {}

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

    pub async fn handle_peer(
        &mut self,
        peer_stream: Box<dyn AsyncReadAndWrite>,
        is_initiator: bool,
    ) -> JoinHandle<()> {
        let mut peer_connection =
            Protocol::with_handshake(Box::into_pin(peer_stream), self.public_key, is_initiator)
                .await;
        info!("Remote pk {:?}", peer_connection.remote_pk.unwrap());

        let requests_to_us_tx = self.requests_to_us_tx.clone();

        let (response_tx, response_rx): (
            async_channel::Sender<OutgoingPeerResponse>,
            async_channel::Receiver<OutgoingPeerResponse>,
        ) = async_channel::unbounded();

        let (requests_from_us_tx, requests_from_us_rx): (
            async_channel::Sender<OutGoingPeerRequest>,
            async_channel::Receiver<OutGoingPeerRequest>,
        ) = async_channel::unbounded();
        self.peers.insert(
            to_hex_string(peer_connection.remote_pk.unwrap()),
            requests_from_us_tx,
        );
        task::spawn(async move {
            let mut response_rx = response_rx.fuse();
            let mut requests_from_us_rx = requests_from_us_rx.fuse();

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
        // if we have no pathbuf and recursive is false, get list of peers
        // if we have no pathbuf, and recursive is true, get list of peers and use it as input
        // if we have a pathbuf, take the root as we do with read_file
        // if path_buf.is_none() && !recursive {
        //     //get list of peers and return
        //     let entries = self
        //         .peers
        //         .keys()
        //         .map(|name| Entry {
        //             name: name.to_string(),
        //             size: 0, // TODO get total size of peers shares
        //             is_dir: true,
        //         })
        //         .collect();
        //     Response::Success(Success {
        //         msg: Some(response::success::Msg::Ls(response::Ls { entries })),
        //     });
        //     // TODO finish
        // }
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
                // let peer = self.peers.get(peer_name).unwrap(); // .ok_or(error);
                // do as in read_file

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
                    ls_entries.push(Entry {
                        name: format!("{}/{}", to_hex_string(public_key), entry.name.clone()),
                        size: entry.size,
                        is_dir: entry.is_dir,
                    });
                }
            };
        }
        ls_entries
    }

    pub async fn request_all(&self) -> Vec<Entry> {
        let mut ls_entries = Vec::new();
        for (_name, peer) in self.peers.iter() {
            let (response_tx, mut response_rx) = async_channel::unbounded();
            peer.send(OutGoingPeerRequest {
                response_tx,
                message: request::Msg::Ls(request::Ls {
                    path: None,
                    searchterm: None,
                    recursive: true,
                }),
            })
            .await
            .unwrap();
            while let Some(res) = response_rx.next().await {
                // TODO if we get an error, return it
                if let IncomingPeerResponse {
                    public_key: _,
                    message:
                        response::Response::Success(Success {
                            msg: Some(response::success::Msg::Ls(response::Ls { entries })),
                        }),
                } = res
                {
                    for entry in entries.iter() {
                        ls_entries.push(entry.clone());
                    }
                };
            }
        }
        ls_entries
    }

    // fn peer_name_from_path(&self, path_buf: PathBuf) -> (&str, &Sender<OutGoingPeerRequest>) {
    //     let peer_name = &path_buf.iter().next().unwrap().to_str().unwrap();
    //     // let remaining_path = path_buf.strip_prefix(peer_name).unwrap();
    //     let peer = self.peers.get(*peer_name).unwrap(); // .ok_or(error);
    //     (peer_name, peer)
    // }

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

    pub async fn request(
        &mut self,
        peer: Sender<OutGoingPeerRequest>,
        request: request::Msg,
    ) -> Result<Receiver<IncomingPeerResponse>, async_channel::SendError<OutGoingPeerRequest>> {
        let (response_tx, response_rx) = async_channel::unbounded();
        peer.send(OutGoingPeerRequest {
            response_tx,
            message: request,
        })
        .await?;
        Ok(response_rx)
    }

    pub async fn run(mut self) {
        self.rpc.run(self.requests_to_us_rx).await;
    }
}

fn to_hex_string(bytes: [u8; 32]) -> String {
    let strs: Vec<String> = bytes.iter().take(2).map(|b| format!("{:02x}", b)).collect();
    strs.join("")
}
