use crate::messages::response::ls::Entry;
use crate::messages::response::{self, Success};
use crate::protocol::{Event, Options, Protocol};
use crate::rpc::Rpc;
use crate::shares::Shares;
use async_channel::{Receiver, Sender};
use async_std::task::{self, JoinHandle};
use futures::io::{AsyncRead, AsyncWrite};
use futures::select;
use futures::StreamExt;
use log::info;
use rand::Rng;
use std::path::Path;

#[derive(Debug)]
pub struct OutGoingPeerRequest {
    pub message: crate::messages::request::Msg,
    pub response_tx: Sender<crate::messages::response::Response>,
}

pub struct PeerRequest {
    pub message: crate::messages::request::Msg,
    pub id: u32,
    // peer_id: String,
    pub response_tx: Sender<PeerResponse>,
}

pub struct PeerResponse {
    pub id: u32,
    pub message: crate::messages::response::Response,
}

pub trait AsyncReadAndWrite: AsyncWrite + AsyncRead + Send + Unpin + 'static {}

pub struct Run {
    peers: Vec<Sender<OutGoingPeerRequest>>,
    pub rpc: Rpc,
    requests_to_us_tx: Sender<PeerRequest>,
    requests_to_us_rx: Receiver<PeerRequest>,
    pub public_key: [u8; 32],
}

impl Run {
    pub async fn new(storage: impl AsRef<Path>) -> Result<Self, crate::shares::CreateSharesError> {
        // TODO this will be replaced by a noise public key
        let mut rng = rand::thread_rng();
        let public_key = rng.gen();
        let shares = Shares::new(storage).await?;
        let rpc = Rpc::new(shares);
        let (requests_tx, requests_rx) = async_channel::unbounded();
        Ok(Self {
            peers: Default::default(),
            rpc,
            requests_to_us_tx: requests_tx,
            requests_to_us_rx: requests_rx,
            public_key,
        })
    }

    pub async fn handle_peer(
        &mut self,
        peer_stream: Box<dyn AsyncReadAndWrite>,
        is_initiator: bool,
    ) -> JoinHandle<()> {
        let mut peer_connection = Protocol::new(
            Box::into_pin(peer_stream),
            Options::new(is_initiator, self.public_key),
        );
        let requests_to_us_tx = self.requests_to_us_tx.clone();

        let (response_tx, response_rx): (
            async_channel::Sender<PeerResponse>,
            async_channel::Receiver<PeerResponse>,
        ) = async_channel::unbounded();

        let (requests_from_us_tx, requests_from_us_rx): (
            async_channel::Sender<OutGoingPeerRequest>,
            async_channel::Receiver<OutGoingPeerRequest>,
        ) = async_channel::unbounded();
        self.peers.push(requests_from_us_tx);
        task::spawn(async move {
            // let peer_connection = peer_connection.fuse();
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
                                requests_to_us_tx
                                    .send(PeerRequest {
                                        message,
                                        id,
                                        response_tx: response_tx.clone(),
                                        // peer_id: "TODO".to_string(),
                                    })
                                .await
                                .unwrap();
                            },
                            // Some(Ok(Event::Response(r))) => {
                            //     info!("processing response {:?}", r);
                            // },
                            _ => {}
                        }
                    },
                    next_res = response_rx.next() => {
                        info!("next res");
                        match next_res {
                            Some(next_res) => {
                                peer_connection
                                    .respond(next_res.message, next_res.id)
                                    .await
                                    .unwrap();
                                },
                            None => {},
                        }
                    },
                    next_req = requests_from_us_rx.next() => {
                        info!("next req {:?} {}", next_req, is_initiator);
                        match next_req {
                            Some(next_req) => {
                                peer_connection.request(next_req).await.unwrap();
                            },
                            None => {},
                        }
                    },
                }
            }
        })
    }

    pub async fn request_all(self) -> Vec<Entry> {
        let mut ls_entries = Vec::new();
        for peer in self.peers.iter() {
            let (response_tx, mut response_rx) = async_channel::unbounded();
            peer.send(OutGoingPeerRequest {
                response_tx,
                message: crate::messages::request::Msg::Ls(crate::messages::request::Ls {
                    path: None,
                    searchterm: None,
                    recursive: None,
                }),
            })
            .await
            .unwrap();
            while let Some(res) = response_rx.next().await {
                // TODO if we get an error, return it
                if let crate::messages::response::Response::Success(Success {
                    msg: Some(response::success::Msg::Ls(response::Ls { entries })),
                }) = res
                {
                    for entry in entries.iter() {
                        ls_entries.push(entry.clone());
                    }
                };
            }
        }
        ls_entries
    }

    // pub async fn request(&mut self, peer_id: &str, request: Request) {
    // find which peer_connection, call peer.request(), do something with the reciever
    // }

    pub async fn run(mut self) {
        self.rpc.run(self.requests_to_us_rx).await;
    }
}
