use crate::fs::ReadStream;
use crate::messages::{request, response};
use crate::shares::Shares;
use futures_lite::Stream;
// use std::collections::VecDeque;
use async_std::fs;

/// Remote Procedure Call - process remote requests (or requests from ourself to query our own
/// share index)
pub struct Rpc {
    shares: Shares,
    // command_queue: VecDeque<Command>,
}

impl Rpc {
    pub fn new(shares: Shares) -> Rpc {
        Rpc {
            shares,
            // command_queue: VecDeque::new(),
        }
    }

    /// Query the filepath index
    async fn ls(
        &mut self,
        path: Option<String>,
        searchterm: Option<String>,
        recursive: Option<bool>,
    ) -> Box<dyn Stream<Item = response::Response> + Send + '_> {
        self.shares.query(path, searchterm, recursive).unwrap()
    }

    /// Read a file, or a section of a file
    async fn read(
        &mut self,
        path: String,
        _start: Option<u64>,
        _end: Option<u64>,
    ) -> Box<dyn Stream<Item = response::Response> + Send + '_> {
        // TODO convert path using share index
        // let resolved_path = self.shares.resolve_path(path).unwrap();
        let file = fs::File::open(path).await.unwrap();
        // TODO pass actual start parameter
        Box::new(ReadStream::new(file, Some(5), None).await.unwrap())
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

    // pub fn run(&mut self, requests_rx: Reciever<PeerRequest>) -> Sender<PeerResponse> {}
}

// fn create_error_stream(_err: Error) -> ResponseStream {
//     let response = response::Response::Err(1);
//     ResponseStream::new_from_ls(stream::iter(vec![response]))
// }

// pub struct Command {
//     req: messages::request::Msg,
//     sender: Sender<messages::response::Response>,
// }

// poll poll_next
// for each active command, call poll_next
// on finishing a command, make the next one on the queue active

// #[cfg(test)]
// mod tests {
//     use super::*;
//
//     #[async_std::test]
//     async fn ls() {
//         let mut r = Rpc::new();
//
//         let req = request::Msg::Ls(request::Ls {
//             path: None,
//             searchterm: None,
//             recursive: None,
//         });
//         let mut s = r.request(req).await;
//         println!("Ls response {:?}", s.next().await);
//     }
//
//     #[async_std::test]
//     async fn read() {
//         let mut r = Rpc::new();
//
//         let req = request::Msg::Read(request::Read {
//             path: "Cargo.toml".to_string(),
//             start: None,
//             end: None,
//         });
//         let mut s = r.request(req).await;
//         println!(" Read response {:?}", s.next().await);
//     }
// }
