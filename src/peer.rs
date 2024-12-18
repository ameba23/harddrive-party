//! Representation of remote peer, and download handling
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    hdp::get_timestamp,
    ui_messages::{DownloadResponse, UiResponse, UiServerMessage},
    wire_messages::{ReadQuery, Request},
    wishlist::{DownloadRequest, RequestedFile, WishList},
};
use anyhow::anyhow;
use bincode::serialize;
use futures::{pin_mut, StreamExt};
use harddrive_party_shared::ui_messages::DownloadInfo;
use key_to_animal::key_to_name;
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

        let peer_name = key_to_name(&public_key);
        // Process requests for this peer in a separate task
        tokio::spawn(async move {
            if let Err(err) = process_requests(
                public_key,
                connection_clone,
                peer_name,
                wishlist,
                download_dir,
                response_tx,
            )
            .await
            {
                error!("Error when processing requests: {:?}", err);
            }
        });

        Self {
            connection,
            public_key,
        }
    }
}

/// Loop over requests for files from this peer
async fn process_requests(
    public_key: [u8; 32],
    connection: Connection,
    peer_name: String,
    wishlist: WishList,
    download_dir: PathBuf,
    response_tx: Sender<UiServerMessage>,
) -> anyhow::Result<()> {
    let request_stream = wishlist.requests_for_peer(&public_key);
    pin_mut!(request_stream);
    // Handle download requests for this peer in serial
    while let Some(mut request) = request_stream.next().await {
        let progress = wishlist
            .get_download_progress_for_request(request.request_id)
            .unwrap_or_default();

        let associated_request = wishlist.get_request(request.request_id)?;
        match download(
            &request,
            &connection,
            &download_dir,
            response_tx.clone(),
            peer_name.clone(),
            progress,
            associated_request.clone(),
        )
        .await
        {
            Ok(()) => {
                debug!("Download successfull");
                request.downloaded = true;
                let id = request.request_id;
                // Mark the file as completed
                match wishlist.file_completed(request) {
                    Ok(request_complete) => {
                        // If all files associated with this request have been downloaded
                        if request_complete {
                            if response_tx
                                .send(UiServerMessage::Response {
                                    id,
                                    response: Ok(UiResponse::Download(DownloadResponse {
                                        path: associated_request.path.clone(),
                                        peer_name: peer_name.clone(),
                                        download_info: DownloadInfo::Completed(get_timestamp()),
                                    })),
                                })
                                .await
                                .is_err()
                            {
                                warn!("Response channel closed");
                            };
                        }
                    }
                    Err(e) => {
                        warn!("Could not remove item from wishlist {:?}", e)
                    }
                }
            }
            Err(e) => {
                warn!("Error downloading {:?}", e);
            }
        }
    }
    Ok(())
}

/// Download a file (or file portion) from the remote peer
async fn download(
    requested_file: &RequestedFile,
    connection: &Connection,
    download_dir: &Path,
    response_tx: Sender<UiServerMessage>,
    peer_name: String,
    progress_request: u64,
    associated_request: DownloadRequest,
) -> anyhow::Result<()> {
    let id = requested_file.request_id;
    let output_path = download_dir.join(requested_file.path.clone());
    let (mut file, start_offset) = setup_download(output_path, requested_file.size).await?;

    // Bytes read from this file
    let mut bytes_read: u64 = start_offset.unwrap_or_default();

    // A running total of all files downloaded in this request
    let mut total_bytes_read = progress_request;

    let mut final_speed = 0;
    if start_offset >= Some(requested_file.size) {
        debug!("File already downloaded");
    } else {
        debug!(
            "Requesting {} from offset {:?}",
            requested_file.path, start_offset
        );

        let mut recv = make_read_request(connection, requested_file, start_offset).await?;
        let mut buf: [u8; DOWNLOAD_BLOCK_SIZE] = [0; DOWNLOAD_BLOCK_SIZE];

        let mut bytes_read_since_last_ui_update = 0;
        let mut speedometer = Speedometer::new(Duration::from_secs(5));

        loop {
            // TODO try reading chunks with offset to avoid head of line blocking
            // let recv_result = recv.read(&mut buf).await;
            match recv.read(&mut buf).await {
                Ok(Some(n)) => {
                    bytes_read_since_last_ui_update += n as u64;
                    speedometer.entry(n);

                    if let Err(error) = file.write(&buf[..n]).await {
                        warn!("Cannot write downloading file {:?}", error);
                        break;
                    }

                    if bytes_read_since_last_ui_update > UPDATE_EVERY {
                        bytes_read += bytes_read_since_last_ui_update;
                        total_bytes_read += bytes_read_since_last_ui_update;
                        if bytes_read > requested_file.size {
                            error!("Downloading file is bigger than expected!");
                        }

                        debug!(
                            "Read {} bytes - {} of {}",
                            bytes_read_since_last_ui_update, bytes_read, requested_file.size
                        );
                        bytes_read_since_last_ui_update = 0;

                        if response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Ok(UiResponse::Download(DownloadResponse {
                                    path: associated_request.path.clone(),
                                    peer_name: peer_name.clone(),
                                    download_info: DownloadInfo::Downloading {
                                        path: requested_file.path.clone(),
                                        bytes_read,
                                        total_bytes_read,
                                        speed: speedometer
                                            .measure()
                                            .unwrap_or_default()
                                            .try_into()
                                            .unwrap(),
                                    },
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
                    final_speed = speedometer
                        .measure()
                        .unwrap_or_default()
                        .try_into()
                        .unwrap();
                    break;
                }
                Err(error) => {
                    error!("Got error {:?}", error);
                    bytes_read += bytes_read_since_last_ui_update;
                    final_speed = speedometer
                        .measure()
                        .unwrap_or_default()
                        .try_into()
                        .unwrap();
                    break;
                }
            }
        }
    }
    // Send a final update to give the UI an accurate report on bytes downloaded
    if response_tx
        .send(UiServerMessage::Response {
            id,
            response: Ok(UiResponse::Download(DownloadResponse {
                peer_name: peer_name.clone(),
                path: associated_request.path.clone(),
                download_info: DownloadInfo::Downloading {
                    path: requested_file.path.clone(),
                    bytes_read,
                    total_bytes_read,
                    speed: final_speed,
                },
            })),
        })
        .await
        .is_err()
    {
        warn!("Response channel closed");
    }

    if bytes_read < requested_file.size {
        return Err(anyhow!(
            "Download incomplete - {} of {} bytes downloaded",
            bytes_read,
            requested_file.size
        ));
    }
    Ok(())
}

/// Send a message requesting a file portion
/// (usually this will be the whole file)
async fn make_read_request(
    connection: &Connection,
    requested_file: &RequestedFile,
    start: Option<u64>,
) -> anyhow::Result<RecvStream> {
    let request = Request::Read(ReadQuery {
        path: requested_file.path.clone(),
        start,
        end: None,
    });

    let (mut send, recv) = connection.open_bi().await?;
    let buf = serialize(&request)?;
    send.write_all(&buf).await?;
    send.finish().await?;
    Ok(recv)
}

/// Setup download and return the file as well as the offset if the file is already partially
/// downloaded
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
