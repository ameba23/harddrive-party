use crate::shares::{EntryParseError, Shares};
use bincode::serialize;
use log::{debug, warn};
use quinn::WriteError;
use thiserror::Error;
use tokio::{
    fs,
    io::{self, AsyncRead, AsyncReadExt, AsyncSeekExt},
};

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
                    let length: u64 = buf.len().try_into().unwrap();
                    debug!("Writing prefix {length}");
                    output.write(&length.to_le_bytes()).await?;

                    output.write(&buf).await?;
                    debug!("Written buf {:?}", buf);
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

    pub async fn read(
        &self,
        path: String,
        start: Option<u64>,
        end: Option<u64>,
        mut output: quinn::SendStream,
    ) -> Result<(), RpcError> {
        match self.get_file_portion(path, start, end).await {
            Ok(file) => {
                // TODO output.write success header
                io::copy(&mut Box::into_pin(file), &mut output).await?;
                output.finish().await?;
                Ok(())
            }
            Err(rpc_error) => send_error(rpc_error, output).await,
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
}

// fn create_error_stream(err: RpcError) -> Box<dyn Stream<Item = response::Response> + Send> {
//     let response = response::Response::Err(err as i32);
//     Box::new(stream::iter(vec![response]))
// }
