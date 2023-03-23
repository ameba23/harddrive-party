use std::{path::PathBuf, time::Duration};

use crate::{
    ui_messages::{ReadResponse, UiResponse, UiServerMessage},
    wire_messages::Request,
};
use anyhow::anyhow;
use bincode::serialize;
use log::{debug, warn};
use quinn::{Connection, RecvStream};
use speedometer::Speedometer;
use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::mpsc::{unbounded_channel, UnboundedSender},
};

const DOWNLOAD_BLOCK_SIZE: usize = 64 * 1024;

#[derive(Debug)]
pub struct DownloadRequest {
    pub path: String,
    pub start: Option<u64>,
    pub end: Option<u64>,
    // This id is not unique - it references which request this came from
    // requesting a directory will be split into requests for each file
    pub id: u32,
}

pub struct Peer {
    pub connection: Connection,
    pub download_request_tx: UnboundedSender<DownloadRequest>,
}

impl Peer {
    pub fn new(
        connection: Connection,
        response_tx: UnboundedSender<UiServerMessage>,
        download_dir: PathBuf,
    ) -> Self {
        let (download_request_tx, mut download_request_rx) = unbounded_channel();

        let connection_clone = connection.clone();
        tokio::spawn(async move {
            // Handle download requests for this peer in serial
            while let Some(request) = download_request_rx.recv().await {
                if let Err(e) = download(
                    request,
                    &connection_clone,
                    &download_dir,
                    response_tx.clone(),
                )
                .await
                {
                    warn!("Error downloading {:?}", e);
                }
            }
        });

        Self {
            connection,
            download_request_tx,
        }
    }
}

async fn download(
    download_request: DownloadRequest,
    connection: &Connection,
    download_dir: &PathBuf,
    response_tx: UnboundedSender<UiServerMessage>,
) -> anyhow::Result<()> {
    let mut recv = make_read_request(connection, &download_request).await?;
    let id = download_request.id;
    // TODO write
    //
    let output_path = download_dir.join(download_request.path.clone());
    match setup_download(output_path, download_request.start).await {
        Ok(mut file) => {
            let mut buf: [u8; DOWNLOAD_BLOCK_SIZE] = [0; DOWNLOAD_BLOCK_SIZE];
            let mut bytes_read: u64 = 0;
            let mut total_bytes_read = 0;
            let mut speedometer = Speedometer::new(Duration::from_secs(5));
            // TODO handle errors here
            while let Ok(Some(n)) = recv.read(&mut buf).await {
                debug!("Read {} bytes", n);
                speedometer.entry(n);
                bytes_read += n as u64;
                total_bytes_read += n as u64;

                if let Err(error) = file.write(&buf[..n]).await {
                    warn!("Cannot write downloading file {:?}", error);
                    break;
                }
                if response_tx
                    .send(UiServerMessage::Response {
                        id,
                        response: Ok(UiResponse::Read(
                            ReadResponse {
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
    //
    //
    //
    Ok(())
}

async fn make_read_request(
    connection: &Connection,
    download_request: &DownloadRequest,
) -> anyhow::Result<RecvStream> {
    let request = Request::Read {
        path: download_request.path.clone(),
        start: download_request.start,
        end: download_request.end,
    };

    let (mut send, recv) = connection.open_bi().await?;
    let buf = serialize(&request)?;
    debug!("Message serialized, writing...");
    send.write_all(&buf).await?;
    send.finish().await?;
    Ok(recv)
}

async fn setup_download(file_path: PathBuf, start: Option<u64>) -> anyhow::Result<File> {
    create_dir_all(
        file_path
            .parent()
            .ok_or_else(|| anyhow!("Cannot get parent"))?,
    )
    .await?;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(file_path)
        .await?;
    if let Some(pos) = start {
        file.seek(std::io::SeekFrom::Start(pos)).await?;
    };
    Ok(file)
}
