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
    sync::mpsc::UnboundedSender,
};

const DOWNLOAD_BLOCK_SIZE: usize = 64 * 1024;

pub struct Peer {
    pub connection: Connection,
    // pub download_request_tx: UnboundedSender<DownloadRequest>,
    pub public_key: [u8; 32],
}

impl Peer {
    pub fn new(
        connection: Connection,
        response_tx: UnboundedSender<UiServerMessage>,
        download_dir: PathBuf,
        public_key: [u8; 32],
        wishlist: WishList,
    ) -> Self {
        let connection_clone = connection.clone();
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
                        if let Err(e) = wishlist.completed(request) {
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
            // download_request_tx,
        }
    }
}

async fn download(
    download_request: &DownloadRequest,
    connection: &Connection,
    download_dir: &Path,
    response_tx: UnboundedSender<UiServerMessage>,
) -> anyhow::Result<()> {
    let id = download_request.request_id;
    // TODO check if the file already exists, and resume
    //
    let output_path = download_dir.join(download_request.path.clone());
    match setup_download(output_path, download_request.size).await {
        Ok((mut file, start_offset)) => {
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
            let mut total_bytes_read = 0;
            let mut speedometer = Speedometer::new(Duration::from_secs(5));
            // TODO handle errors here
            loop {
                let recv_result = recv.read(&mut buf).await;
                match recv_result {
                    Ok(Some(n)) => {
                        speedometer.entry(n);
                        bytes_read += n as u64;
                        total_bytes_read += n as u64;

                        if bytes_read > download_request.size {
                            error!("Downloading file is bigger than expected!");
                        }

                        if let Err(error) = file.write(&buf[..n]).await {
                            warn!("Cannot write downloading file {:?}", error);
                            break;
                        }
                        debug!(
                            "Read {} bytes - {} of {}",
                            n, bytes_read, download_request.size
                        );
                        if response_tx
                            .send(UiServerMessage::Response {
                                id,
                                response: Ok(UiResponse::Download(
                                    DownloadResponse {
                                        path: download_request.path.clone(),
                                        bytes_read,
                                        total_bytes_read,
                                        speed: speedometer.measure().unwrap(),
                                    },
                                    // buf[..n].to_vec(),
                                )),
                            })
                            .is_err()
                        {
                            warn!("Response channel closed");
                            break;
                        };
                    }
                    Ok(None) => {
                        debug!("Stream ended");
                        break;
                    }
                    Err(error) => {
                        error!("Got error {:?}", error);
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
        }
        Err(error) => {
            warn!("Cannot setup output file for download {:?}", error);
            return Err(error);
        }
    };
    Ok(())
}

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

    let metadata = file.metadata().await?;
    let existing_file_size = metadata.len();

    let start_offset = if existing_file_size > size {
        error!("Existing file is bigger than the remote source");
        Some(existing_file_size)
    } else {
        match existing_file_size {
            0 => None,
            _ => Some(size - existing_file_size),
        }
    };

    // if let Some(pos) = start {
    //     file.seek(std::io::SeekFrom::Start(pos)).await?;
    // };
    Ok((file, start_offset))
}
