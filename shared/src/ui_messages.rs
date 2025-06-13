//! Messages for communicating with the user interface over websocket

use crate::wire_messages::IndexQuery;
use serde::{Deserialize, Serialize};
use std::{fmt, time::Duration};
use thiserror::Error;

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct FilesQuery {
    pub peer_name: Option<String>,
    pub query: IndexQuery,
}

/// 'Events' are messages sent from the server which are not in response to a particular
/// request - eg: to inform the UI that a peer connected or disconnected
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum UiEvent {
    /// A peer has connected
    PeerConnected { name: String },
    /// A peer has disconnected
    PeerDisconnected { name: String },
    /// A peer connection failed
    PeerConnectionFailed { name: String, error: String },
    /// Part of a file has been uploaded
    Uploaded(UploadInfo),
    /// Download
    Download(DownloadEvent),
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Info {
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
    /// Name of peer who holds file
    pub peer_name: String,
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
    pub speed: usize,
    pub peer_name: String,
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
    /// Name of the peer who holds the file or directory
    pub peer_name: String,
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
                    self.peer_name, path, bytes_read, total_bytes_read, speed
                )
            }
            DownloadInfo::Completed(_time) => {
                write!(f, "Completed {}/{}", self.peer_name, self.path)
            }
        }
    }
}

/// Represents a remote file
#[derive(Serialize, Deserialize, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeerPath {
    /// The name of the peer who holds the file
    pub peer_name: String,
    /// The path to the remote file
    pub path: String,
}
