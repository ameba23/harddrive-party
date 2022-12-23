use crate::messages::Request;
use crate::protocol::{Event, Options, Protocol};
use crate::rpc::Rpc;
use async_channel::{Receiver, Sender};
use async_std::sync::Mutex;
use async_std::task::{self, JoinHandle};
use futures_lite::io::{AsyncRead, AsyncWrite};
use futures_lite::StreamExt;
use std::sync::Arc;

struct PeerRequest {
    message: crate::messages::request::Msg,
    id: u32,
    peer_id: String,
}

pub struct Run<IO> {
    peers: Arc<Mutex<Vec<Protocol<IO>>>>,
    rpc: Arc<Mutex<Rpc>>,
    requests_tx: Sender<PeerRequest>,
}

impl<IO> Run<IO>
where
    IO: AsyncWrite + AsyncRead + Send + Unpin + 'static,
{
    pub fn new(rpc: Arc<Mutex<Rpc>>) -> Self {
        let (requests_tx, requests_rx) = async_channel::unbounded();
        // TODO give requests_rx to rpc
        Self {
            peers: Default::default(),
            rpc,
            requests_tx,
        }
    }

    pub async fn handle_peer(&mut self, peer_stream: IO, is_initiator: bool) {
        let mut peer_connection = Protocol::new(peer_stream, Options::new(is_initiator));
        let peers = self.peers.clone();
        // peers.push(peer_connection);
        // TODO we need a channel pair to send requests to / get responses from the rpc
        //
        {
            // let requests_tx = self.requests_tx.clone();
            // let peers = peers.clone();
            let rpc = self.rpc.clone();
            task::spawn(async move {
                while let Some(next_event) = peer_connection.next().await {
                    match next_event {
                        Ok(Event::Request(message, id)) => {
                            println!("got request {:?} {}", message, id);
                            // requests_tx
                            //     .send(PeerRequest {
                            //         message,
                            //         id,
                            //         peer_id: "TODO".to_string(),
                            //     })
                            //     .await
                            //     .unwrap();
                            let mut locked_rpc = rpc.lock().await;
                            let mut responses = Box::into_pin(locked_rpc.request(message).await);
                            while let Some(res) = responses.next().await {
                                peer_connection.respond(res, id).await.unwrap();
                            }
                            // peer_connection.responsd(endresponse);
                        }
                        _ => {}
                    }
                }
            });
        }
    }

    pub async fn request(&mut self, peer_id: &str, request: Request) {
        // find which peer_connection, call peer.request(), do something with the reciever
    }
}
