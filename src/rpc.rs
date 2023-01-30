use crate::shares::Shares;
use bincode::serialize;
// use log::debug;
use thiserror::Error;
use tokio::{
    fs,
    io::{self, AsyncReadExt, AsyncSeekExt},
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
        &self,
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
