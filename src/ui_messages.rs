//! Messages for communicating with the user interface over websocket

use crate::wire_messages::{IndexQuery, LsResponse, ReadQuery};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    net::SocketAddr,
    time::{Duration, SystemTime},
};
use thiserror::Error;

/// A command from the Ui
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
    /// Directly connected to a peer with the given socketaddr (temporary)
    Connect(SocketAddr),
    /// Query peers' files
    Ls(IndexQuery, Option<String>),
    /// Read a portion a of a file
    Read(ReadQuery, String),
    /// Download a file or dir
    Download { path: String, peer_name: String },
    /// Query our own shares
    Shares(IndexQuery),
    /// Shutdown gracefully
    Close,
}

/// A message to the UI
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum UiServerMessage {
    Response {
        id: u32,
        response: Result<UiResponse, UiServerError>,
    },
    Event(UiEvent),
}

/// 'Events' are messages sent from the server which are not in response to a particular
/// request - eg: to inform the UI that a peer connected or disconnected
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum UiEvent {
    PeerConnected {
        name: String,
        is_self: bool,
    },
    PeerDisconnected {
        name: String,
    },
    Uploaded(UploadInfo),
    ConnectedTopics(Vec<String>),
    Wishlist {
        requested: Vec<DownloadRequest>,
        downloaded: Vec<DownloadRequest>,
    },
}

/// A requested file
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct DownloadRequest {
    /// The path of the file on the remote
    pub path: String,
    /// The size in bytes
    pub size: u64,
    /// This id is not unique - it references which request this came from
    /// requesting a directory will be split into requests for each file
    pub request_id: u32,
    /// Time when request made relative to unix epoch
    pub timestamp: Duration,
    /// Public key of peer holding file
    pub peer_public_key: [u8; 32],
}

impl DownloadRequest {
    pub fn new(path: String, size: u64, request_id: u32, peer_public_key: [u8; 32]) -> Self {
        let system_time = SystemTime::now();
        let timestamp = system_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Time went backwards");
        // let timestamp = seconds * 1000 + (nanos % 1_000_000_000) / 1_000_000;
        Self {
            path,
            size,
            request_id,
            timestamp,
            peer_public_key,
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct UploadInfo {
    pub path: String,
    pub bytes_read: u64,
    pub speed: usize,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum UiResponse {
    Download(DownloadResponse),
    Read(Vec<u8>),
    Ls(LsResponse, String),
    Shares(LsResponse),
    Connect,
    EndResponse,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Error, Clone)]
pub enum UiServerError {
    #[error("Cannot connect")]
    ConnectionError,
    #[error("Request error")]
    RequestError, // TODO
    #[error("Error when joining or leaving")]
    JoinOrLeaveError,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct DownloadResponse {
    pub path: String,
    pub bytes_read: u64,
    pub total_bytes_read: u64,
    pub speed: usize,
}

impl fmt::Display for DownloadResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} bytes read, {} bytes per second.",
            self.path, self.bytes_read, self.speed
        )
    }
}
