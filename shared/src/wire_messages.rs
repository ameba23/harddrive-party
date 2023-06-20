//! Wire messages for communicating with other Peers
use serde::{Deserialize, Serialize};
use thiserror::Error;

// TODO read error

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub enum Request {
    Ls(IndexQuery),
    Read(ReadQuery),
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Eq, Hash)]
pub struct ReadQuery {
    pub path: String,
    pub start: Option<u64>,
    pub end: Option<u64>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Eq, Hash)]
pub struct IndexQuery {
    pub path: Option<String>,
    pub searchterm: Option<String>,
    pub recursive: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum LsResponse {
    Success(Vec<Entry>),
    Err(LsResponseError),
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Error, Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum LsResponseError {
    #[error("Database error")]
    DbError,
    #[error("Path not found")]
    PathNotFound,
}
