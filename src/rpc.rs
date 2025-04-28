//! Remote procedure call for share index queries and file uploading

use crate::{
    shares::{EntryParseError, Shares},
    ui_messages::{UiEvent, UiServerMessage, UploadInfo},
};
use bincode::{deserialize, serialize};
use harddrive_party_shared::wire_messages::{AnnouncePeer, IndexQuery, ReadQuery, Request};
use log::{debug, error, warn};
use quinn::WriteError;
use thiserror::Error;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt},
    sync::mpsc::{channel, Receiver, Sender},
};

/// Number of bytes uploaded at a time
const UPLOAD_BLOCK_SIZE: usize = 64 * 1024;

struct ReadRequest {
    path: String,
    start: Option<u64>,
    end: Option<u64>,
    output: quinn::SendStream,
    requester_name: String,
}

/// Remote Procedure Call - process remote requests
#[derive(Clone)]
pub struct Rpc {
    /// The file index database
    pub shares: Shares,
    /// Channel for sending upload requests
    upload_tx: Sender<ReadRequest>,
    // upload_rx: UnboundedReceiver<ReadRequest>,
    peer_announce_tx: Sender<AnnouncePeer>,
}

impl Rpc {
    pub fn new(
        shares: Shares,
        event_tx: Sender<UiServerMessage>,
        peer_announce_tx: Sender<AnnouncePeer>,
    ) -> Rpc {
        let (upload_tx, upload_rx) = channel(65536);
        let shares_clone = shares.clone();

        tokio::spawn(async move {
            let mut uploader = Uploader {
                shares: shares_clone,
                upload_rx,
                event_tx,
            };
            uploader.run().await;
        });

        Rpc {
            shares,
            upload_tx,
            peer_announce_tx,
        }
    }

    /// Handle a request
    pub async fn request(&self, buf: Vec<u8>, output: quinn::SendStream, peer_name: String) {
        let request: Result<Request, Box<bincode::ErrorKind>> = deserialize(&buf);
        match request {
            Ok(req) => {
                debug!("Got request from peer {:?}", req);
                match req {
                    Request::Ls(IndexQuery {
                        path,
                        searchterm,
                        recursive,
                    }) => {
                        if let Ok(()) = self.ls(path, searchterm, recursive, output).await {};
                        // TODO else
                    }
                    Request::Read(ReadQuery { path, start, end }) => {
                        if let Ok(()) = self.read(path, start, end, output, peer_name).await {};
                        // TODO else
                    }
                    Request::AnnouncePeer(announce_peer) => {
                        log::info!(
                            "Discovered peer through existing peer connection {announce_peer:?}"
                        );
                        self.peer_announce_tx.send(announce_peer).await.unwrap();
                    }
                }
            }
            Err(_) => {
                warn!("Cannot decode wire message");
            }
        }
    }

    /// Query the filepath index
    async fn ls(
        &self,
        path: Option<String>,
        searchterm: Option<String>,
        recursive: bool,
        mut output: quinn::SendStream,
    ) -> Result<(), RpcError> {
        println!("Responding to ls query");
        match self.shares.query(path, searchterm, recursive) {
            Ok(response_iterator) => {
                for res in response_iterator {
                    let buf = serialize(&res).map_err(|e| {
                        error!("Cannot serialize query response {:?}", e);
                        RpcError::SerializeError
                    })?;

                    // Write the length prefix
                    // TODO this should be a varint
                    let length: u64 = buf
                        .len()
                        .try_into()
                        .map_err(|_| RpcError::U64ConvertError)?;
                    debug!("Writing prefix {length}");
                    output.write_all(&length.to_le_bytes()).await?;

                    output.write_all(&buf).await?;
                    debug!("Written ls response");
                }
                output.finish().await?;
                Ok(())
            }
            Err(error) => {
                warn!("Error during share query {:?}", error);
                send_error(
                    match error {
                        EntryParseError::PathNotFound => RpcError::PathNotFound,
                        _ => RpcError::DbError,
                    },
                    output,
                )
                .await
            }
        }
    }

    /// Read a portion of a file
    /// This puts the read request, which contains a stream for the response, in a queue
    async fn read(
        &self,
        path: String,
        start: Option<u64>,
        end: Option<u64>,
        output: quinn::SendStream,
        requester_name: String,
    ) -> Result<(), RpcError> {
        self.upload_tx
            .send(ReadRequest {
                path,
                start,
                end,
                output,
                requester_name,
            })
            .await
            .map_err(|_| RpcError::ChannelClosed)?;
        Ok(())
    }
}

/// Uploads are processed sequentially in a separate task
struct Uploader {
    /// Our share db
    shares: Shares,
    /// Incoming read requests
    upload_rx: Receiver<ReadRequest>,
    /// Channel for sending messages to the UI
    event_tx: Sender<UiServerMessage>,
}

impl Uploader {
    /// Iterate over incoming read requests
    async fn run(&mut self) {
        while let Some(read_request) = self.upload_rx.recv().await {
            if let Err(e) = self.do_read(read_request).await {
                warn!("Error uploading {:?}", e);
            }
        }
    }

    /// Upload a file portion
    async fn do_read(&self, read_request: ReadRequest) -> Result<(), RpcError> {
        // Possibly here we could adjust the block size based on conjestion
        // by using output.write and checking the number of bytes written in one go

        let ReadRequest {
            path,
            start,
            end,
            mut output,
            requester_name,
        } = read_request;
        match self.get_file_portion(path.clone(), start, end).await {
            Ok((file, size)) => {
                // TODO output.write success header
                // io::copy(&mut Box::into_pin(file), &mut output).await?;
                let mut buf: [u8; UPLOAD_BLOCK_SIZE] = [0; UPLOAD_BLOCK_SIZE];
                let mut file = Box::into_pin(file);
                let mut bytes_read: u64 = start.unwrap_or(0);
                while let Ok(n) = file.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    bytes_read += n as u64;
                    if self
                        .event_tx
                        .send(UiServerMessage::Event(UiEvent::Uploaded(UploadInfo {
                            path: path.clone(),
                            bytes_read,
                            speed: 0, // TODO
                            peer_name: requester_name.clone(),
                        })))
                        .await
                        .is_err()
                    {
                        warn!("Ui response channel closed");
                    };
                    output.write_all(&buf[..n]).await?;
                    debug!("Uploaded {} bytes of {}", bytes_read, size);
                }
                output.finish().await?;
                Ok(())
            }
            Err(rpc_error) => send_error(rpc_error, output).await,
        }
    }
    // This is what write_all does internally:
    // loop {
    //     if this.buf.is_empty() {
    //         return Poll::Ready(Ok(()));
    //     }
    //     let buf = this.buf;
    //     let n = ready!(this.stream.execute_poll(cx, |s| s.write(buf)))?;
    //     this.buf = &this.buf[n..];
    // }

    /// Get the potion of the file we want to upload
    async fn get_file_portion(
        &self,
        path: String,
        start: Option<u64>,
        end: Option<u64>,
    ) -> Result<(Box<dyn AsyncRead + Send>, u64), RpcError> {
        let start = start.unwrap_or(0);

        if let Some(end_offset) = end {
            if start > end_offset {
                return Err(RpcError::BadOffset);
            }
        }

        match self.shares.resolve_path(path) {
            Ok((resolved_path, size)) => match fs::File::open(resolved_path).await {
                Ok(mut file) => {
                    // Check file size matches what we have in the db
                    let metadata = file.metadata().await?;
                    let size_on_disk = metadata.len();
                    if size_on_disk != size {
                        error!("File size does not match that from db!");
                    }

                    // Seek to start point
                    file.seek(std::io::SeekFrom::Start(start))
                        .await
                        .map_err(|_| RpcError::BadOffset)?;

                    // If an endpoint is specified, only read until endpoint
                    match end {
                        Some(endpoint) => {
                            // TODO should this be just endpoint?
                            Ok((Box::new(file.take(start + endpoint)), size))
                        }
                        None => Ok((Box::new(file), size)),
                    }
                }
                Err(_) => Err(RpcError::PathNotFound),
            },
            Err(_) => Err(RpcError::PathNotFound),
        }
    }
}

/// Respond with an error
async fn send_error(error: RpcError, mut output: quinn::SendStream) -> Result<(), RpcError> {
    // TODO send serialised version of the error
    output.write_all(&[0]).await?;
    output.finish().await?;
    Err(error)
}

/// Error to be sent as a wire message
#[derive(Error, Debug)]
pub enum RpcError {
    #[error("Db error")]
    DbError,
    #[error("Path not found")]
    PathNotFound,
    #[error("Bad offset")]
    BadOffset,
    #[error("Read error")]
    ReadError(#[from] std::io::Error),
    #[error("Write error")]
    WriteError(#[from] WriteError),
    #[error("serialize error")]
    SerializeError,
    #[error("Channel closed")]
    ChannelClosed,
    #[error("Cannot convert to u64")]
    U64ConvertError,
}

// fn create_error_stream(err: RpcError) -> Box<dyn Stream<Item = response::Response> + Send> {
//     let response = response::Response::Err(err as i32);
//     Box::new(stream::iter(vec![response]))
// }
