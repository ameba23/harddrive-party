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

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum Command {
    Connect(SocketAddr),
    Request(Request, String),
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
    PeerConnected,
    PeerDisconnected,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum UiResponse {
    Read(Vec<u8>),
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
