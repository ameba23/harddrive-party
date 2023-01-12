use crate::fs::ReadStream;
use crate::messages::response::{success, EndResponse, Success};
use crate::messages::{request, response};
use crate::run::{PeerRequest, PeerResponse};
use crate::shares::Shares;
use async_channel::Receiver;
use async_std::fs;
use futures::{stream, Stream, StreamExt};
use log::info;

/// Remote Procedure Call - process remote requests (or requests from ourself to query our own
/// share index)
pub struct Rpc {
    pub shares: Shares,
    // command_queue: VecDeque<Command>,
}

impl Rpc {
    pub fn new(shares: Shares) -> Rpc {
        Rpc {
            shares,
            // command_queue: VecDeque::new(),
            // requests_rx: Reciever<PeerRequest>
        }
    }

    /// Query the filepath index
    async fn ls(
        &mut self,
        path: Option<String>,
        searchterm: Option<String>,
        recursive: Option<bool>,
    ) -> Box<dyn Stream<Item = response::Response> + Send + '_> {
        match self.shares.query(path, searchterm, recursive) {
            Ok(e) => e,
            Err(_) => create_error_stream(1),
        }
    }

    /// Read a file, or a section of a file
    async fn read(
        &mut self,
        path: String,
        start: Option<u64>,
        end: Option<u64>,
    ) -> Box<dyn Stream<Item = response::Response> + Send + '_> {
        // TODO this error for cannot resolve path
        let resolved_path = self.shares.resolve_path(path).unwrap();

        // TODO return an error if start > end

        // TODO this error for file doesnt exist
        let file = fs::File::open(resolved_path).await.unwrap();

        // TODO this error if the initial seek fails because start > filesize
        Box::new(ReadStream::new(file, start, end).await.unwrap())
    }

    pub async fn request(
        &mut self,
        req: request::Msg,
    ) -> Box<dyn Stream<Item = response::Response> + Send + '_> {
        match req {
            request::Msg::Ls(request::Ls {
                path,
                searchterm,
                recursive,
            }) => self.ls(path, searchterm, recursive).await,
            request::Msg::Read(request::Read { path, start, end }) => {
                self.read(path, start, end).await
            }
            request::Msg::Handshake(_) => self.ls(None, None, None).await,
        }
        // let (tx, rx) = async_channel::unbounded();
        // self.command_queue.push_back(Command { req, sender: tx });
        //
        // // self.ls(None, None, None, tx);
        // rx
    }

    pub async fn run(&mut self, mut requests_rx: Receiver<PeerRequest>) {
        if let Some(peer_request) = requests_rx.next().await {
            let mut responses = Box::into_pin(self.request(peer_request.message).await);
            while let Some(res) = responses.next().await {
                info!("*** response");
                peer_request
                    .response_tx
                    .send(PeerResponse {
                        message: res,
                        id: peer_request.id,
                    })
                    .await
                    .unwrap();
            }
            // Finally send an endresponse
            peer_request
                .response_tx
                .send(PeerResponse {
                    id: peer_request.id,
                    message: crate::messages::response::Response::Success(Success {
                        msg: Some(success::Msg::EndResponse(EndResponse {})),
                    }),
                })
                .await
                .unwrap();
        }
    }
}

fn create_error_stream(err: i32) -> Box<dyn Stream<Item = response::Response> + Send> {
    let response = response::Response::Err(err);
    Box::new(stream::iter(vec![response]))
}

// pub struct Command {
//     req: messages::request::Msg,
//     sender: Sender<messages::response::Response>,
// }

// poll poll_next
// for each active command, call poll_next
// on finishing a command, make the next one on the queue active
