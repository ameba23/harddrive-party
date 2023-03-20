use crate::wire_messages::{LsResponse, Request};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use thiserror::Error;

/// Messages for communicating with the user interface over websocket

/// A command from the Ui
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct UiClientMessage {
    pub id: u32,
    pub command: Command,
}

/// A command from the UI
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum Command {
    /// Directly connected to a peer with the given socketaddr (temporary)
    Connect(SocketAddr),
    /// Issue a request
    Request(Request, String),
    /// Download a file or dir
    Download { entry: String, peer_name: String },
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

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum UiEvent {
    PeerConnected(String),
    PeerDisconnected(String),
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
    Read(ReadResponse),
    Ls(LsResponse, String),
    Connect,
    EndResponse,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Error, Clone)]
pub enum UiServerError {
    #[error("Cannot connect")]
    ConnectionError,
    #[error("Request error")]
    RequestError, // TODO
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct ReadResponse {
    pub path: String,
    pub bytes_read: u64,
    pub total_bytes_read: u64,
    pub speed: usize,
}
