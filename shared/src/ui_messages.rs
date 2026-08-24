//! Messages for communicating with the user interface over websocket

use crate::{announce_address::AnnounceAddressDecodeError, wire_messages::IndexQuery, PeerId};
use serde::{Deserialize, Serialize};
use std::{fmt, time::Duration};
use thiserror::Error;

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct FilesQuery {
    pub peer_id: Option<PeerId>,
    pub query: IndexQuery,
}

/// Canonical peer identity plus its derived human-readable label.
#[derive(Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Hash)]
pub struct PeerInfo {
    pub id: PeerId,
    pub name: String,
}

impl PeerInfo {
    pub fn from_id(id: PeerId) -> Self {
        Self {
            id,
            name: key_to_animal::key_to_name(id.as_bytes()),
        }
    }
}

/// 'Events' are messages sent from the server which are not in response to a particular
/// request - eg: to inform the UI that a peer connected or disconnected
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum UiEvent {
    /// A peer has connected
    PeerConnected { peer: PeerInfo },
    /// A peer has disconnected
    PeerDisconnected { peer: PeerInfo, error: String },
    /// A peer connection failed
    PeerConnectionFailed { peer: PeerInfo, error: String },
    /// Part of a file has been uploaded
    Uploaded(UploadInfo),
    /// Download
    Download(DownloadEvent),
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Info {
    pub id: PeerId,
    pub name: String,
    pub os_home_dir: Option<String>,
    pub announce_address: String,
}

/// A request to download a file from a particular peer
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct UiDownloadRequest {
    /// The path of the file on the remote
    pub path: String,
    /// How much is already downloaded
    pub progress: u64,
    /// The total size in bytes
    pub total_size: u64,
    /// Identifier for the request
    pub request_id: u32,
    /// Time when request made relative to unix epoch
    pub timestamp: Duration,
    /// Peer who holds the file.
    pub peer: PeerInfo,
    /// Whether the requested path is a directory.
    pub is_dir: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct UiRequestedFile {
    pub path: String,
    /// The size in bytes
    pub size: u64,
    /// This id is not unique - it references which request this came from
    /// requesting a directory will be split into requests for each file
    pub request_id: u32,
    pub downloaded: bool,
}

/// Information about a current running upload
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct UploadInfo {
    pub path: String,
    pub bytes_read: u64,
    pub total_size: u64,
    pub speed: usize,
    pub peer: PeerInfo,
}

/// An error in response to a UI command
#[derive(Serialize, Deserialize, PartialEq, Debug, Error, Clone)]
pub enum UiServerError {
    #[error("Cannot connect: {0}")]
    ConnectionError(String),
    #[error("Request error: {0}")]
    RequestError(String),
    #[error("Error when updating shared directory")]
    ShareError(String),
    #[error("Serialization: {0}")]
    Serialization(String),
    #[error("Peer discovery: {0}")]
    PeerDiscovery(String),
    #[error("Poisoned lock")]
    Poison,
    #[error("Database: {0}")]
    Db(String),
    #[error("Error adding directory to share: {0}")]
    AddShare(String),
    #[error("Cannot decode announce address {0}")]
    AnnounceAddressDecode(#[from] AnnounceAddressDecodeError),
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum DownloadInfo {
    Downloading {
        /// File path of currently downloading file
        path: String,
        /// Number of bytes read for this file
        bytes_read: u64,
        /// Total number of bytes read from the associated download request
        total_bytes_read: u64,
        /// Current speed of download in bytes per second
        speed: u32,
    },
    Completed(Duration),
}

/// A response to a download request
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct DownloadEvent {
    pub request_id: u32,
    /// File path of requested file of directory
    pub path: String,
    /// Peer who holds the file or directory.
    pub peer: PeerInfo,
    pub download_info: DownloadInfo,
    // pub total_size: u64,
}

impl fmt::Display for DownloadEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.download_info {
            DownloadInfo::Downloading {
                path,
                bytes_read,
                total_bytes_read,
                speed,
            } => {
                write!(
                    f,
                    "Downloading {}/{} {} bytes read, {} total bytes read, {} bps",
                    self.peer.name, path, bytes_read, total_bytes_read, speed
                )
            }
            DownloadInfo::Completed(_time) => {
                write!(f, "Completed {}/{}", self.peer.name, self.path)
            }
        }
    }
}

/// Represents a remote file
#[derive(Serialize, Deserialize, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeerPath {
    /// The peer who holds the file.
    pub peer: PeerInfo,
    /// The path to the remote file
    pub path: String,
}
