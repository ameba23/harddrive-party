use crate::{
    shares::{EntryParseError, Shares},
    ui_messages::{UiEvent, UiServerMessage, UploadInfo},
};
use bincode::serialize;
use log::{debug, warn};
use quinn::WriteError;
use thiserror::Error;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt},
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
};

const UPLOAD_BLOCK_SIZE: usize = 64 * 1024;

struct ReadRequest {
    path: String,
    start: Option<u64>,
    end: Option<u64>,
    output: quinn::SendStream,
}

/// Remote Procedure Call - process remote requests (or requests from ourself to query our own
/// share index)
pub struct Rpc {
    pub shares: Shares,
    upload_tx: UnboundedSender<ReadRequest>,
    // upload_rx: UnboundedReceiver<ReadRequest>,
}

impl Rpc {
    pub fn new(shares: Shares, event_tx: UnboundedSender<UiServerMessage>) -> Rpc {
        // TODO this should also take a tx for sending upload events to the UI
        let (upload_tx, upload_rx) = unbounded_channel();
        let shares_clone = shares.clone();

        tokio::spawn(async move {
            let mut uploader = Uploader {
                shares: shares_clone,
                upload_rx,
                event_tx,
            };
            uploader.run().await;
        });

        Rpc { shares, upload_tx }
    }

    /// Query the filepath index
    pub async fn ls(
        &self,
        path: Option<String>,
        searchterm: Option<String>,
        recursive: bool,
        mut output: quinn::SendStream,
    ) -> Result<(), RpcError> {
        match self.shares.query(path, searchterm, recursive) {
            Ok(it) => {
                for res in it {
                    let buf = serialize(&res).map_err(|e| {
                        warn!("Cannot serialize query response {:?}", e);
                        RpcError::SerializeError
                    })?;

                    // Write the length prefix
                    // TODO this should be a varint
                    let length: u64 = buf
                        .len()
                        .try_into()
                        .map_err(|_| RpcError::U64ConvertError)?;
                    debug!("Writing prefix {length}");
                    output.write(&length.to_le_bytes()).await?;
                    // TODO display a warning if length would be more than u32::MAX - so that
                    // the conversion back to usize will work on 32 bit machines

                    output.write(&buf).await?;
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

    pub async fn read(
        &self,
        path: String,
        start: Option<u64>,
        end: Option<u64>,
        output: quinn::SendStream,
    ) -> Result<(), RpcError> {
        self.upload_tx
            .send(ReadRequest {
                path,
                start,
                end,
                output,
            })
            .map_err(|_| RpcError::ChannelClosed)?;
        Ok(())
    }
}

/// Uploads are processed sequentially in a separate task
struct Uploader {
    shares: Shares,
    upload_rx: UnboundedReceiver<ReadRequest>,
    event_tx: UnboundedSender<UiServerMessage>,
}

impl Uploader {
    async fn run(&mut self) {
        while let Some(read_request) = self.upload_rx.recv().await {
            if let Err(e) = self.do_read(read_request).await {
                warn!("Error uploading {:?}", e);
            }
        }
    }

    async fn do_read(&self, read_request: ReadRequest) -> Result<(), RpcError> {
        let ReadRequest {
            path,
            start,
            end,
            mut output,
        } = read_request;
        match self.get_file_portion(path.clone(), start, end).await {
            Ok(file) => {
                // TODO output.write success header
                // io::copy(&mut Box::into_pin(file), &mut output).await?;
                let mut buf: [u8; UPLOAD_BLOCK_SIZE] = [0; UPLOAD_BLOCK_SIZE];
                let mut file = Box::into_pin(file);
                let mut bytes_read: u64 = 0; // TODO this should depend on start
                while let Ok(n) = file.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    debug!("Uploaded {} bytes", n);
                    bytes_read += n as u64;
                    if self
                        .event_tx
                        .send(UiServerMessage::Event(UiEvent::Uploaded(UploadInfo {
                            path: path.clone(),
                            bytes_read,
                            speed: 0, // TODO
                        })))
                        .is_err()
                    {
                        warn!("Ui response channel closed");
                    };
                    output.write(&buf[..n]).await?;
                }
                output.finish().await?;
                Ok(())
            }
            Err(rpc_error) => send_error(rpc_error, output).await,
        }
    }

    async fn get_file_portion(
        &self,
        path: String,
        start: Option<u64>,
        end: Option<u64>,
    ) -> Result<Box<dyn AsyncRead + Send>, RpcError> {
        let start = start.unwrap_or(0);

        if let Some(end_offset) = end {
            if start > end_offset {
                return Err(RpcError::BadOffset);
            }
        }

        match self.shares.resolve_path(path) {
            Ok(resolved_path) => match fs::File::open(resolved_path).await {
                Ok(mut file) => {
                    file.seek(std::io::SeekFrom::Start(start))
                        .await
                        .map_err(|_| RpcError::BadOffset)?;
                    match end {
                        Some(e) => {
                            // TODO should this be just e?
                            Ok(Box::new(file.take(start + e)))
                        }
                        None => Ok(Box::new(file)),
                    }
                }
                Err(_) => Err(RpcError::PathNotFound),
            },
            Err(_) => Err(RpcError::PathNotFound),
        }
    }
}

async fn send_error(error: RpcError, mut output: quinn::SendStream) -> Result<(), RpcError> {
    // TODO send serialised version of the error
    output.write(&[0]).await?;
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
