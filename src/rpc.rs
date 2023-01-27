// use crate::fs::ReadStream;
// use crate::messages::response::{success, EndResponse, Success};
// use crate::messages::{request, response};
// use crate::run::{IncomingPeerRequest, OutgoingPeerResponse};
use crate::shares::{EntryParseError, Shares};
use bincode::serialize;
use tokio::{
    fs,
    io::{self, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
};
// use futures::{stream, Stream, StreamExt};
use log::info;
use thiserror::Error;

/// Remote Procedure Call - process remote requests (or requests from ourself to query our own
/// share index)
pub struct Rpc {
    pub shares: Shares,
}

impl Rpc {
    pub fn new(shares: Shares) -> Rpc {
        Rpc { shares }
    }

    /// Query the filepath index
    pub async fn ls(
        &mut self,
        path: Option<String>,
        searchterm: Option<String>,
        recursive: bool,
        mut output: quinn::SendStream,
    ) -> () {
        match self.shares.query(path, searchterm, recursive) {
            Ok(it) => {
                for res in it {
                    let buf = serialize(&res).unwrap();
                    output.write(&buf).await.unwrap();
                }
                output.finish().await.unwrap();
            }
            Err(_) => {}
        }
    }

    pub async fn read(
        &mut self,
        path: String,
        start: Option<u64>,
        end: Option<u64>,
        mut output: quinn::SendStream,
    ) -> Result<(), RpcError> {
        let start = start.unwrap_or(0);

        if let Some(end_offset) = end {
            if start > end_offset {
                return Err(RpcError::BadOffset);
            }
        }

        match self.shares.resolve_path(path) {
            Ok(resolved_path) => match fs::File::open(resolved_path).await {
                Ok(mut file) => {
                    file.seek(std::io::SeekFrom::Start(start)).await.unwrap();
                    match end {
                        Some(e) => {
                            io::copy(&mut file.take(start + e), &mut output)
                                .await
                                .unwrap();
                        }
                        None => {
                            // io::copy(&mut file, &mut output).await.unwrap();
                            let mut buf: [u8; 32] = [0; 32];
                            let n = file.read(&mut buf).await.unwrap();
                            output.write(&buf[..n]).await.unwrap();
                            println!("copied {}", n);
                            output.finish().await.unwrap();
                        }
                    }
                    Ok(())
                }
                Err(_) => Err(RpcError::PathNotFound),
            },
            Err(_) => Err(RpcError::PathNotFound),
        }
    }
    // pub async fn read(
    //     &mut self,
    //     path: String,
    //     start: Option<u64>,
    //     end: Option<u64>,
    //     output: impl AsyncWrite,
    // ) -> Result<Box<dyn AsyncRead + Send + 'static>> {
    //     let start = start.unwrap_or(0);
    //
    //     if let Some(end_offset) = end {
    //         if start > end_offset {
    //             return create_error_stream(RpcError::BadOffset);
    //         }
    //     }
    //
    //     match self.shares.resolve_path(path) {
    //         Ok(resolved_path) => {
    //             match fs::File::open(resolved_path).await {
    //                 Ok(file) => {
    //                     match ReadStream::new(file, start, end).await {
    //                         Ok(rs) => Box::new(rs),
    //                         // If the initial seek fails because start > filesize
    //                         Err(_) => create_error_stream(RpcError::BadOffset),
    //                     }
    //                 }
    //                 Err(_) => create_error_stream(RpcError::PathNotFound),
    //             }
    //         }
    //         Err(_) => create_error_stream(RpcError::PathNotFound),
    //     }
    // }
    //
    // /// Read a file, or a section of a file
    // async fn read(
    //     &mut self,
    //     path: String,
    //     start: Option<u64>,
    //     end: Option<u64>,
    // ) -> Box<dyn AsyncRead + Send + '_> {
    //     let start = start.unwrap_or(0);
    //
    //     if let Some(end_offset) = end {
    //         if start > end_offset {
    //             return create_error_stream(RpcError::BadOffset);
    //         }
    //     }
    //
    //     match self.shares.resolve_path(path) {
    //         Ok(resolved_path) => {
    //             match fs::File::open(resolved_path).await {
    //                 Ok(file) => {
    //                     match ReadStream::new(file, start, end).await {
    //                         Ok(rs) => Box::new(rs),
    //                         // If the initial seek fails because start > filesize
    //                         Err(_) => create_error_stream(RpcError::BadOffset),
    //                     }
    //                 }
    //                 Err(_) => create_error_stream(RpcError::PathNotFound),
    //             }
    //         }
    //         Err(_) => create_error_stream(RpcError::PathNotFound),
    //     }
    // }
    //
    // // TODO this should be private, but it is used in the test
    // pub async fn request(
    //     &mut self,
    //     req: request::Msg,
    // ) -> Box<dyn Stream<Item = response::Response> + Send + '_> {
    //     match req {
    //         request::Msg::Ls(request::Ls {
    //             path,
    //             searchterm,
    //             recursive,
    //         }) => self.ls(path, searchterm, recursive).await,
    //         request::Msg::Read(request::Read { path, start, end }) => {
    //             self.read(path, start, end).await
    //         }
    //         request::Msg::Handshake(_) => self.ls(None, None, true).await,
    //     }
    // }
    //
    // /// Loop serving requests
    // /// Will return an error it gets a channel which is no longer open
    // pub async fn run(
    //     &mut self,
    //     mut requests_rx: Receiver<IncomingPeerRequest>,
    // ) -> Result<(), SendError<OutgoingPeerResponse>> {
    //     while let Some(peer_request) = requests_rx.next().await {
    //         let mut responses = Box::into_pin(self.request(peer_request.message).await);
    //         let mut is_error = false;
    //         while let Some(res) = responses.next().await {
    //             info!("Sending response");
    //             is_error = matches!(res, response::Response::Err(_));
    //             peer_request
    //                 .response_tx
    //                 .send(OutgoingPeerResponse {
    //                     message: res,
    //                     id: peer_request.id,
    //                 })
    //                 .await?;
    //         }
    //         // Finally send an endresponse, unless we already sent an error
    //         if !is_error {
    //             peer_request
    //                 .response_tx
    //                 .send(OutgoingPeerResponse {
    //                     id: peer_request.id,
    //                     message: crate::messages::response::Response::Success(Success {
    //                         msg: Some(success::Msg::EndResponse(EndResponse {})),
    //                     }),
    //                 })
    //                 .await?;
    //         }
    //     }
    //     Ok(())
    // }
}

// ENOENT: -2,
// ENOTDIR: -20,
// EHOSTUNREACH: -113,
// EEXIST: -17,
// EBUSY: -16,
// ENOLCK: -39
/// Error code to be send as a wire message
#[repr(i32)]
#[derive(Error, Debug)]
pub enum RpcError {
    #[error("Db error")]
    DbError = 1,
    #[error("Path not found")]
    PathNotFound = -2,
    #[error("Bad offset")]
    BadOffset = 2,
}

// fn create_error_stream(err: RpcError) -> Box<dyn Stream<Item = response::Response> + Send> {
//     let response = response::Response::Err(err as i32);
//     Box::new(stream::iter(vec![response]))
// }
