//! Messages for communicating with the user interface over websocket

use crate::wire_messages::{IndexQuery, LsResponse, ReadQuery};
use serde::{Deserialize, Serialize};
use std::{fmt, net::SocketAddr};
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
    PeerConnected { name: String, is_self: bool },
    PeerDisconnected { name: String },
    Uploaded(UploadInfo),
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
