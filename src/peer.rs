//! Representation of remote peer, and download handling
use std::{
    io::ErrorKind,
    num::NonZeroUsize,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    connections::{get_timestamp, speedometer::Speedometer},
    ui_messages::{DownloadEvent, DownloadInfo, PeerInfo, UiEvent},
    wire_messages::{AnnouncePeer, ReadQuery, Request},
    wishlist::{DownloadRequest, RequestedFile, WishList},
};
use anyhow::anyhow;
use futures::{pin_mut, StreamExt};
use harddrive_party_shared::{codec::serialize, wire_messages::Entry, PeerId};
use log::{debug, error, warn};
use lru::LruCache;
use quinn::{Connection, RecvStream};
use std::sync::{Arc, Mutex};
use tokio::{
    fs::{create_dir, create_dir_all, symlink_metadata, File, OpenOptions},
    io::AsyncWriteExt,
    sync::broadcast,
};

// Maybe this is too big - not sure if it matters as this is only allocated
// once per download
pub const DOWNLOAD_BLOCK_SIZE: usize = 64 * 1024;

// Minimum data delta before sending a progress update to the UI.
const MIN_UI_UPDATE_BYTES: u64 = 256 * 1024;
// Minimum time between progress updates when transferring quickly.
const MIN_UI_UPDATE_INTERVAL: Duration = Duration::from_millis(300);
// Maximum time between progress updates on slow links.
const MAX_UI_UPDATE_INTERVAL: Duration = Duration::from_secs(2);

/// The number of records which will be cached when doing index (`Ls`) queries to a remote peer
/// This saves making subsequent requests with a duplicate query
const CACHE_SIZE: usize = 64;

/// The cache for index requests
type IndexCache = LruCache<Request, Vec<Vec<Entry>>>;

/// Representation of a remote peer
#[derive(Debug)]
pub struct Peer {
    /// The QUIC connection to this peer
    pub connection: Connection,
    /// The peer's public ed25519 key
    pub public_key: PeerId,
    /// The peer's verified, gossip-authorized announcement, if received.
    pub announcement: Option<AnnouncePeer>,
    /// Cache for peer's file index, to avoid making duplicate requests
    pub index_cache: Arc<Mutex<IndexCache>>,
}

impl Peer {
    pub fn new(
        connection: Connection,
        event_broadcaster: broadcast::Sender<UiEvent>,
        download_dir: PathBuf,
        public_key: PeerId,
        wishlist: WishList,
        announcement: Option<AnnouncePeer>,
    ) -> Self {
        let connection_clone = connection.clone();

        let peer_info = PeerInfo::from_id(public_key);
        // Process requests for this peer in a separate task
        tokio::spawn(async move {
            if let Err(err) = process_requests(
                public_key,
                connection_clone,
                peer_info,
                wishlist,
                download_dir,
                event_broadcaster,
            )
            .await
            {
                error!("Error when processing requests: {err:?}");
            }
        });

        Self {
            connection,
            public_key,
            announcement,
            index_cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(CACHE_SIZE).expect("Cache size to be non-zero"),
            ))),
        }
    }
}

/// Loop over requests for files from this peer
async fn process_requests(
    public_key: PeerId,
    connection: Connection,
    peer_info: PeerInfo,
    wishlist: WishList,
    download_dir: PathBuf,
    event_broadcaster: broadcast::Sender<UiEvent>,
) -> anyhow::Result<()> {
    let request_stream = wishlist.requests_for_peer(public_key.as_bytes());
    pin_mut!(request_stream);
    // Handle download requests for this peer in serial
    while let Some(mut request) = request_stream.next().await {
        if let Some(reason) = connection.close_reason() {
            debug!(
                "Stopping request processing for {} because the connection is closed: {}",
                peer_info.name, reason
            );
            break;
        }

        let progress = wishlist
            .get_download_progress_for_request(request.request_id)
            .unwrap_or_default();

        let associated_request = wishlist.get_request(request.request_id)?;
        match download(
            &request,
            &connection,
            &download_dir,
            event_broadcaster.clone(),
            peer_info.clone(),
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
                        // TODO here we could also send an EndResponse message
                        if request_complete
                            && event_broadcaster
                                .send(UiEvent::Download(DownloadEvent {
                                    request_id: id,
                                    path: associated_request.path.clone(),
                                    peer: peer_info.clone(),
                                    download_info: DownloadInfo::Completed(get_timestamp()),
                                }))
                                .is_err()
                        {
                            warn!("No UI listeners for completed download event");
                        };
                    }
                    Err(e) => {
                        warn!("Could not remove item from wishlist {e:?}")
                    }
                }
            }
            Err(e) => {
                warn!("Error downloading {e:?}");
                if let Some(reason) = connection.close_reason() {
                    debug!(
                        "Stopping request processing for {} after download error because the connection is closed: {}",
                        peer_info.name, reason
                    );
                    break;
                }
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
    event_broadcaster: broadcast::Sender<UiEvent>,
    peer_info: PeerInfo,
    progress_request: u64,
    associated_request: DownloadRequest,
) -> anyhow::Result<()> {
    let id = requested_file.request_id;
    let (mut file, start_offset) =
        setup_download(download_dir, &requested_file.path, requested_file.size).await?;

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
        let mut last_ui_update = Instant::now();

        loop {
            // TODO try reading chunks with offset to avoid head of line blocking
            // let recv_result = recv.read(&mut buf).await;
            match recv.read(&mut buf).await {
                Ok(Some(n)) => {
                    bytes_read_since_last_ui_update += n as u64;
                    speedometer.entry(n);

                    if let Err(error) = file.write(&buf[..n]).await {
                        warn!("Cannot write downloading file {error:?}");
                        break;
                    }

                    let elapsed = last_ui_update.elapsed();
                    let should_emit = (bytes_read_since_last_ui_update >= MIN_UI_UPDATE_BYTES
                        && elapsed >= MIN_UI_UPDATE_INTERVAL)
                        || elapsed >= MAX_UI_UPDATE_INTERVAL;
                    if should_emit {
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
                        last_ui_update = Instant::now();

                        if event_broadcaster
                            .send(UiEvent::Download(DownloadEvent {
                                request_id: id,
                                path: associated_request.path.clone(),
                                peer: peer_info.clone(),
                                download_info: DownloadInfo::Downloading {
                                    path: requested_file.path.clone(),
                                    bytes_read,
                                    total_bytes_read,
                                    speed: speedometer.measure().try_into().unwrap_or_default(),
                                },
                            }))
                            .is_err()
                        {
                            warn!(
                                "No UI listeners for download progress on {}",
                                requested_file.path
                            );
                        };
                    }
                }
                Ok(None) => {
                    debug!("Stream ended");
                    bytes_read += bytes_read_since_last_ui_update;
                    final_speed = speedometer.measure().try_into().unwrap_or_default();
                    break;
                }
                Err(error) => {
                    error!("Got error {error:?}");
                    bytes_read += bytes_read_since_last_ui_update;
                    final_speed = speedometer.measure().try_into().unwrap_or_default();
                    break;
                }
            }
        }
    }
    // Send a final update to give the UI an accurate report on bytes downloaded
    if event_broadcaster
        .send(UiEvent::Download(DownloadEvent {
            request_id: id,
            peer: peer_info.clone(),
            path: associated_request.path.clone(),
            download_info: DownloadInfo::Downloading {
                path: requested_file.path.clone(),
                bytes_read,
                total_bytes_read,
                speed: final_speed,
            },
        }))
        .is_err()
    {
        warn!(
            "No UI listeners for final download progress on {}",
            requested_file.path
        );
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

/// Convert a peer-provided path into a safe relative local path.
///
/// Remote file indexes are untrusted. Only normal relative path components are
/// accepted so a peer cannot write outside the configured download directory.
pub(crate) fn safe_remote_relative_path(path: &str) -> anyhow::Result<PathBuf> {
    let mut relative_path = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => relative_path.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(anyhow!("Remote path contains an unsafe component: {path}"));
            }
        }
    }
    Ok(relative_path)
}

pub(crate) fn remote_path_is_within_request(requested_path: &Path, entry_path: &Path) -> bool {
    requested_path.as_os_str().is_empty() || entry_path.starts_with(requested_path)
}

fn output_path_for_remote_path(download_dir: &Path, remote_path: &str) -> anyhow::Result<PathBuf> {
    let relative_path = safe_remote_relative_path(remote_path)?;
    if relative_path.as_os_str().is_empty() {
        return Err(anyhow!("Remote file path is empty"));
    }
    Ok(download_dir.join(relative_path))
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
    send.finish()?;
    Ok(recv)
}

/// Setup download and return the file as well as the offset if the file is already partially
/// downloaded
async fn setup_download(
    download_dir: &Path,
    remote_path: &str,
    size: u64,
) -> anyhow::Result<(File, Option<u64>)> {
    let relative_path = safe_remote_relative_path(remote_path)?;
    if relative_path.as_os_str().is_empty() {
        return Err(anyhow!("Remote file path is empty"));
    }

    let file_path = output_path_for_remote_path(download_dir, remote_path)?;
    ensure_download_root(download_dir).await?;
    ensure_safe_parent_dirs(
        download_dir,
        relative_path.parent().unwrap_or_else(|| Path::new("")),
    )
    .await?;
    reject_existing_symlink_or_directory(&file_path).await?;

    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&file_path)
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

/// Ensure the configured download root exists and is a real directory
async fn ensure_download_root(download_dir: &Path) -> anyhow::Result<()> {
    match symlink_metadata(download_dir).await {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => create_dir_all(download_dir).await?,
        Err(err) => return Err(err.into()),
    }

    ensure_existing_directory(download_dir).await
}

/// Create and validate each parent directory component for a download target
async fn ensure_safe_parent_dirs(
    download_dir: &Path,
    relative_parent: &Path,
) -> anyhow::Result<()> {
    let mut current = download_dir.to_path_buf();
    for component in relative_parent.components() {
        let Component::Normal(part) = component else {
            return Err(anyhow!("Remote path contains an unsafe parent component"));
        };

        current.push(part);
        match symlink_metadata(&current).await {
            Ok(_) => {
                ensure_existing_directory(&current).await?;
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                match create_dir(&current).await {
                    Ok(()) => {}
                    Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(err.into()),
                }

                ensure_existing_directory(&current).await?;
            }
            Err(err) => return Err(err.into()),
        }
    }

    Ok(())
}

/// Ensure an existing path is a real directory and not a symlink
async fn ensure_existing_directory(path: &Path) -> anyhow::Result<()> {
    let metadata = symlink_metadata(path).await?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "Download path directory is a symlink: {}",
            path.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(anyhow!(
            "Download path directory is not a directory: {}",
            path.display()
        ));
    }
    Ok(())
}

/// Reject an existing final download target if it is a symlink or directory
async fn reject_existing_symlink_or_directory(path: &Path) -> anyhow::Result<()> {
    match symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(anyhow!("Download target is a symlink: {}", path.display()))
        }
        Ok(metadata) if metadata.is_dir() => Err(anyhow!(
            "Download target is a directory: {}",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_remote_relative_path_accepts_normal_relative_paths() {
        assert_eq!(
            safe_remote_relative_path("share/subdir/file.txt").unwrap(),
            PathBuf::from("share").join("subdir").join("file.txt")
        );
    }

    #[test]
    fn safe_remote_relative_path_rejects_traversal_and_absolute_paths() {
        for path in ["../outside", "share/../../outside", "/tmp/outside", "."] {
            assert!(
                safe_remote_relative_path(path).is_err(),
                "expected {path:?} to be rejected"
            );
        }
    }

    #[test]
    fn remote_path_scope_is_component_based() {
        let requested = safe_remote_relative_path("share").unwrap();
        let child = safe_remote_relative_path("share/subdir/file.txt").unwrap();
        let sibling_with_prefix = safe_remote_relative_path("share-other/file.txt").unwrap();

        assert!(remote_path_is_within_request(&requested, &child));
        assert!(!remote_path_is_within_request(
            &requested,
            &sibling_with_prefix
        ));
    }

    #[test]
    fn output_path_for_remote_path_rejects_empty_and_unsafe_paths() {
        let download_dir = PathBuf::from("/tmp/downloads");

        assert!(output_path_for_remote_path(&download_dir, "").is_err());
        assert!(output_path_for_remote_path(&download_dir, "../../escape").is_err());
        assert_eq!(
            output_path_for_remote_path(&download_dir, "share/file.txt").unwrap(),
            download_dir.join("share").join("file.txt")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn setup_download_rejects_symlink_parent() {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        symlink(outside.path(), tempdir.path().join("link")).unwrap();

        assert!(setup_download(tempdir.path(), "link/file.txt", 10)
            .await
            .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn setup_download_rejects_symlink_target() {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::TempDir::new().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), tempdir.path().join("file.txt")).unwrap();

        assert!(setup_download(tempdir.path(), "file.txt", 10)
            .await
            .is_err());
    }
}
