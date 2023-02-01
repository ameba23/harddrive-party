use crate::hdp::RequestError;
use crate::wire_messages::{LsResponse, Request};
use std::net::SocketAddr;
use thiserror::Error;

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

#[derive(Debug, PartialEq)]
pub enum UiResponse {
    Read(Vec<u8>),
    Ls(LsResponse),
    Connect,
    EndResponse,
}

#[derive(Error, Debug)]
pub enum UiServerError {
    #[error(transparent)]
    RequestError(#[from] RequestError),
    #[error("Cannot connect")]
    ConnectionError,
}

#[derive(Debug)]
pub struct UiServerMessage {
    pub id: u32,
    pub response: Result<UiResponse, UiServerError>,
}
