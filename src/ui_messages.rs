use crate::hdp::RequestError;
use crate::wire_messages::{LsResponse, Request};
use std::net::SocketAddr;
use thiserror::Error;

/// Messages for communicating with the user interface over websocket

/// A command from the Ui
#[derive(Debug)]
pub struct UiClientMessage {
    pub id: u32,
    pub command: Command,
}

#[derive(Debug)]
pub enum Command {
    Connect(SocketAddr),
    Request(Request, String),
    Close,
}

/// A message to the UI
#[derive(Debug)]
pub enum UiServerMessage {
    Response {
        id: u32,
        response: Result<UiResponse, UiServerError>,
    },
    Event(UiEvent),
}

#[derive(Debug, PartialEq)]
pub enum UiEvent {
    PeerConnected,
    PeerDisconnected,
}

#[derive(Debug, PartialEq)]
pub enum UiResponse {
    Read(Vec<u8>),
    Ls(LsResponse, String),
    Connect,
    EndResponse,
}

#[derive(Error, Debug, PartialEq)]
pub enum UiServerError {
    #[error(transparent)]
    RequestError(#[from] RequestError),
    #[error("Cannot connect")]
    ConnectionError,
}
