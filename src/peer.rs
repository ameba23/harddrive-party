//! Representation of remote peer, and download handling
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    ui_messages::{DownloadResponse, UiResponse, UiServerMessage},
    wire_messages::{ReadQuery, Request},
    wishlist::{DownloadRequest, WishList},
};
use anyhow::anyhow;
use bincode::serialize;
use futures::{pin_mut, StreamExt};
use log::{debug, error, warn};
use quinn::{Connection, RecvStream};
use speedometer::Speedometer;
use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::AsyncWriteExt,
    sync::mpsc::Sender,
};

// Maybe this is too big - not sure if it matters as this is only allocated
// once per download
const DOWNLOAD_BLOCK_SIZE: usize = 64 * 1024;

// How often (in bytes) to update the UI on process during downloading
const UPDATE_EVERY: u64 = 10 * 1024;

/// Representation of a remote peer
pub struct Peer {
    /// The QUIC connection to this peer
    pub connection: Connection,
    /// The peer's public ed25519 key
    pub public_key: [u8; 32],
}

impl Peer {
    pub fn new(
        connection: Connection,
        response_tx: Sender<UiServerMessage>,
        download_dir: PathBuf,
        public_key: [u8; 32],
        wishlist: WishList,
    ) -> Self {
        let connection_clone = connection.clone();

        // Loop over requests for files from this peer
        tokio::spawn(async move {
            let request_stream = wishlist.requests_for_peer(&public_key);
            pin_mut!(request_stream);
            // Handle download requests for this peer in serial
            while let Some(request) = request_stream.next().await {
                match download(
                    &request,
                    &connection_clone,
                    &download_dir,
                    response_tx.clone(),
                )
                .await
                {
                    Ok(()) => {
                        debug!("Download successfull");
                        if let Err(e) = wishlist.completed(request).await {
                            warn!("Could not remove item from wishlist {:?}", e)
                        }
                    }
                    Err(e) => {
                        warn!("Error downloading {:?}", e);
                    }
                }
            }
        });

        Self {
            connection,
            public_key,
        }
    }
}

/// Download a file (or file portion) from the remote peer
async fn download(
    download_request: &DownloadRequest,
    connection: &Connection,
    download_dir: &Path,
    response_tx: Sender<UiServerMessage>,
) -> anyhow::Result<()> {
    let id = download_request.request_id;
    let output_path = download_dir.join(download_request.path.clone());
    let (mut file, start_offset) = setup_download(output_path, download_request.size).await?;

    if start_offset == Some(download_request.size) {
        debug!("File already downloaded");
        return Ok(());
    }

    debug!(
        "Requesting {} from offset {:?}",
        download_request.path, start_offset
    );

    let mut recv = make_read_request(connection, download_request, start_offset).await?;
    let mut buf: [u8; DOWNLOAD_BLOCK_SIZE] = [0; DOWNLOAD_BLOCK_SIZE];
    let mut bytes_read: u64 = 0;

    // TODO total bytes read should be a running total of all files downloaded in this
    // request
    let mut total_bytes_read = 0;

    let mut bytes_read_since_last_ui_update = 0;
    let mut speedometer = Speedometer::new(Duration::from_secs(5));

    loop {
        // TODO try reading chunks with offset to avoid head of line blocking
        // let recv_result = recv.read(&mut buf).await;
        match recv.read(&mut buf).await {
            Ok(Some(n)) => {
                bytes_read_since_last_ui_update += n as u64;

                if let Err(error) = file.write(&buf[..n]).await {
                    warn!("Cannot write downloading file {:?}", error);
                    break;
                }

                if bytes_read_since_last_ui_update > UPDATE_EVERY {
                    bytes_read += bytes_read_since_last_ui_update;
                    total_bytes_read += bytes_read_since_last_ui_update;
                    if bytes_read > download_request.size {
                        error!("Downloading file is bigger than expected!");
                    }

                    debug!(
                        "Read {} bytes - {} of {}",
                        bytes_read_since_last_ui_update, bytes_read, download_request.size
                    );
                    bytes_read_since_last_ui_update = 0;

                    speedometer.entry(n);

                    if response_tx
                        .send(UiServerMessage::Response {
                            id,
                            response: Ok(UiResponse::Download(DownloadResponse {
                                path: download_request.path.clone(),
                                bytes_read,
                                total_bytes_read,
                                speed: speedometer.measure().unwrap_or_default(),
                            })),
                        })
                        .await
                        .is_err()
                    {
                        warn!("Response channel closed");
                        break;
                    };
                }
            }
            Ok(None) => {
                debug!("Stream ended");
                bytes_read += bytes_read_since_last_ui_update;
                break;
            }
            Err(error) => {
                error!("Got error {:?}", error);
                bytes_read += bytes_read_since_last_ui_update;
                break;
            }
        }
    }

    if bytes_read < download_request.size {
        return Err(anyhow!(
            "Download incomplete - {} of {} bytes downloaded",
            bytes_read,
            download_request.size
        ));
    }
    // Terminate with an endresponse
    // if self.response_tx
    //     .send(UiServerMessage::Response {
    //         id,
    //         response: Ok(UiResponse::EndResponse),
    //     })
    //     .is_err()
    // {
    //     warn!("Response channel closed");
    // }
    Ok(())
}

/// Send a message requesting a file portion
/// (usually this will be the whole file)
async fn make_read_request(
    connection: &Connection,
    download_request: &DownloadRequest,
    start: Option<u64>,
) -> anyhow::Result<RecvStream> {
    let request = Request::Read(ReadQuery {
        path: download_request.path.clone(),
        start,
        end: None,
    });

    let (mut send, recv) = connection.open_bi().await?;
    let buf = serialize(&request)?;
    send.write_all(&buf).await?;
    send.finish().await?;
    Ok(recv)
}

async fn setup_download(file_path: PathBuf, size: u64) -> anyhow::Result<(File, Option<u64>)> {
    // Create directory to put the downloaded file in
    create_dir_all(
        file_path
            .parent()
            .ok_or_else(|| anyhow!("Cannot get parent"))?,
    )
    .await?;

    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(file_path)
        .await?;

    // If the file already exists, start writing where we left off
    let metadata = file.metadata().await?;
    let existing_file_size = metadata.len();

    let start_offset = if existing_file_size > size {
        error!("Existing file is bigger than the remote source");
        // Treat as already downloaded (don't clobber existing file)
        // TODO probably should return an error here
        Some(size)
    } else {
        match existing_file_size {
            0 => None,
            _ => Some(existing_file_size),
        }
    };

    Ok((file, start_offset))
}
