use crate::fs::ReadStream;
use crate::messages::{request, response};
use futures_lite::stream::{self, StreamExt};
use futures_lite::Stream;
// use std::collections::VecDeque;
use async_std::fs;
use std::io::Error;
use std::pin::Pin;
use std::task::{Context, Poll};

// pub struct Command {
//     req: messages::request::Msg,
//     sender: Sender<messages::response::Response>,
// }

pub enum InnerResponseStream {
    Ls(stream::Iter<std::vec::IntoIter<response::Response>>),
    Read(ReadStream),
}

pub struct ResponseStream {
    inner: InnerResponseStream,
}

impl ResponseStream {
    pub fn new(inner: InnerResponseStream) -> Self {
        ResponseStream { inner }
    }

    pub fn new_from_ls(inner: stream::Iter<std::vec::IntoIter<response::Response>>) -> Self {
        ResponseStream {
            inner: InnerResponseStream::Ls(inner),
        }
    }
    pub fn new_from_read(inner: ReadStream) -> Self {
        ResponseStream {
            inner: InnerResponseStream::Read(inner),
        }
    }
}

impl Stream for ResponseStream {
    type Item = response::Response;
    fn poll_next(
        mut self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
    ) -> Poll<Option<<Self as Stream>::Item>> {
        match self.inner {
            InnerResponseStream::Ls(ref mut i) => i.poll_next(ctx),
            InnerResponseStream::Read(ref mut i) => i.poll_next(ctx),
        }
    }
}

pub struct Rpc {
    // command_queue: VecDeque<Command>,
}

impl Rpc {
    pub fn new() -> Rpc {
        Rpc {
            // command_queue: VecDeque::new(),
        }
    }

    async fn ls(
        &mut self,
        _path: Option<String>,
        _searchterm: Option<String>,
        _recursive: Option<bool>,
    ) -> ResponseStream {
        let entry = response::ls::Entry {
            name: String::from("somefile"),
            size: 1000,
            is_dir: false,
        };
        let response = response::Response::Success(response::Success {
            msg: Some(response::success::Msg::Ls(response::Ls {
                entries: vec![entry],
            })),
        });
        ResponseStream::new_from_ls(stream::iter(vec![response]))
    }

    async fn read(
        &mut self,
        path: String,
        _start: Option<u64>,
        _end: Option<u64>,
    ) -> ResponseStream {
        let file = fs::File::open(path).await.unwrap();
        let rs = ReadStream::new(file, Some(5), None).await.unwrap();
        ResponseStream::new_from_read(rs)
    }

    pub async fn request(&mut self, req: request::Msg) -> ResponseStream {
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
}

fn create_error_stream(_err: Error) -> ResponseStream {
    let response = response::Response::Err(1);
    ResponseStream::new_from_ls(stream::iter(vec![response]))
}

// poll poll_next
// for each active command, call poll_next
// on finishing a command, make the next one on the queue active

#[cfg(test)]
mod tests {
    use super::*;

    #[async_std::test]
    async fn ls() {
        let mut r = Rpc::new();

        let req = request::Msg::Ls(request::Ls {
            path: None,
            searchterm: None,
            recursive: None,
        });
        let mut s = r.request(req).await;
        println!("Ls response {:?}", s.next().await);
    }

    #[async_std::test]
    async fn read() {
        let mut r = Rpc::new();

        let req = request::Msg::Read(request::Read {
            path: "Cargo.toml".to_string(),
            start: None,
            end: None,
        });
        let mut s = r.request(req).await;
        println!(" Read response {:?}", s.next().await);
    }
}
