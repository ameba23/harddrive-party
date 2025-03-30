//! Messages for communicating with the user interface over websocket

use crate::wire_messages::{IndexQuery, LsResponse, ReadQuery};
use serde::{Deserialize, Serialize};
use std::{fmt, time::Duration};
use thiserror::Error;

/// A command from the UI together with a random ID used to refer to it
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct UiClientMessage {
    pub id: u32,
    pub command: Command,
}

/// A command from the UI
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum Command {
    /// Join a given topic name
    Join(String),
    /// Leave a given topic name
    Leave(String),
    /// Query peers' files. If no peer name is given, query all connected peers
    Ls(IndexQuery, Option<String>),
    /// Read a portion a of a file, from the given connect peer name
    Read(ReadQuery, String),
    /// Download a file or dir
    Download {
        path: String,
        peer_name: String,
    },
    /// Query our own shares
    Shares(IndexQuery),
    /// Add or update a directory to share
    AddShare(String),
    /// Stop sharing a directory
    RemoveShare(String),
    /// Get download requests
    Requests,
    /// Get files associated with a particular request
    RequestedFiles(u32),
    RemoveRequest(u32),
    /// Connect to the given peer
    ConnectDirect(Vec<u8>),
    /// Shutdown gracefully
    Close,
}

/// A message to the UI
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum UiServerMessage {
    /// The response to a `Command`
    Response {
        id: u32,
        response: Result<UiResponse, UiServerError>,
    },
    /// Some server message which is not responding to a particular command
    Event(UiEvent),
}

/// 'Events' are messages sent from the server which are not in response to a particular
/// request - eg: to inform the UI that a peer connected or disconnected
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum UiEvent {
    /// A peer has connected
    PeerConnected {
        name: String,
        /// Is the peer me? This is used to pass our own details to the UI
        peer_type: PeerRemoteOrSelf,
    },
    /// A peer has disconnected
    PeerDisconnected { name: String },
    /// Part of a file has been uploaded
    Uploaded(UploadInfo),
    /// The topics connected to has changed
    /// Topic name, connected?, announce address
    Topics(Vec<(String, bool, Option<Vec<u8>>)>),
    // /// The requested or downloaded files have changed
    // Wishlist {
    //     requested: Vec<UiDownloadRequest>,
    //     downloaded: Vec<UiDownloadRequest>,
    // },
}

/// Details of a [UiEvent::PeerConnected] indicating whether the connecting peer is ourself or a
/// remote peer.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum PeerRemoteOrSelf {
    /// A remote peer
    Remote,
    /// Ourself, with the path of our home directory
    Me { os_home_dir: Option<String> },
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

/// Response to a UI command
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum UiResponse {
    Download(DownloadResponse),
    Read(Vec<u8>),
    Ls(LsResponse, String),
    Shares(LsResponse),
    AddShare(u32),
    RemoveShare,
    Requests(Vec<UiDownloadRequest>),
    RequestedFiles(Vec<UiRequestedFile>),
    EndResponse,
}

/// An error in response to a UI command
#[derive(Serialize, Deserialize, PartialEq, Debug, Error, Clone)]
pub enum UiServerError {
    #[error("Cannot connect")]
    ConnectionError,
    #[error("Request error")]
    RequestError, // TODO
    #[error("Error when joining or leaving")]
    JoinOrLeaveError,
    #[error("Error when updating shared directory")]
    ShareError(String),
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum DownloadInfo {
    Requested(Duration),
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
pub struct DownloadResponse {
    /// File path of requested file of directory
    pub path: String,
    /// Name of the peer who holds the file or directory
    pub peer_name: String,
    pub download_info: DownloadInfo,
    // pub total_size: u64,
}

impl fmt::Display for DownloadResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.download_info {
            DownloadInfo::Requested(_time) => {
                write!(f, "Requested {}/{}", self.peer_name, self.path)
            }
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
