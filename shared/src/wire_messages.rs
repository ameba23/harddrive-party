//! Wire messages for communicating with other Peers
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use thiserror::Error;

// TODO read error

/// A request to a remote peer
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub enum Request {
    /// A request to read the remote peer's shared file index
    Ls(IndexQuery),
    /// A request to download a remote peer's file (or a portion of the file)
    Read(ReadQuery),
    /// Contact details of another peer
    AnnouncePeer(AnnouncePeer),
}

/// A request to read the remote peer's shared file index
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Eq, Hash)]
pub struct IndexQuery {
    /// Base directory to query - defaults to all shared directories
    pub path: Option<String>,
    /// Filter term to search with
    pub searchterm: Option<String>,
    /// Whether to expand directories
    pub recursive: bool,
}

/// A request to download a remote peers file (or a portion of the file)
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Eq, Hash)]
pub struct ReadQuery {
    /// Path of the requested file
    pub path: String,
    /// Offset to start reading
    pub start: Option<u64>,
    /// Offset to finish reading
    pub end: Option<u64>,
}

/// A response to a `Request::Ls(IndexQuery)`
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum LsResponse {
    /// The found files or directories if the query was successful
    Success(Vec<Entry>),
    Err(LsResponseError),
}

/// A file or directory entry in a share query response
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Entry {
    /// Path and filename
    pub name: String,
    /// Size in bytes
    pub size: u64,
    /// Whether this is a directory or a file
    pub is_dir: bool,
}

/// Error from making a share index query
#[derive(Error, Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum LsResponseError {
    #[error("Database error")]
    DbError,
    #[error("Path not found")]
    PathNotFound,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub struct AnnouncePeer {
    pub connection_details: PeerConnectionDetails,
    // signature
}

#[repr(u8)]
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub enum PeerConnectionDetails {
    NoNat(SocketAddr) = 1,
    Asymmetric(SocketAddr) = 2,
    Symmetric(IpAddr) = 3,
}

impl PeerConnectionDetails {
    /// Gets the IP address
    pub fn ip(&self) -> IpAddr {
        match self {
            PeerConnectionDetails::NoNat(addr) => addr.ip(),
            PeerConnectionDetails::Asymmetric(addr) => addr.ip(),
            PeerConnectionDetails::Symmetric(ip) => *ip,
        }
    }
}
