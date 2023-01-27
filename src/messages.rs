use serde::{Deserialize, Serialize};
use thiserror::Error;

// TODO read error

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum Request {
    Ls {
        path: Option<String>,
        searchterm: Option<String>,
        recursive: bool,
    },
    Read {
        path: String,
        start: Option<u64>,
        end: Option<u64>,
    },
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum LsResponse {
    Success(Vec<Entry>),
    Err(LsResponseError),
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

#[derive(Error, Serialize, Deserialize, PartialEq, Debug)]
pub enum LsResponseError {
    #[error("Database error")]
    DbError,
    #[error("Path not found")]
    PathNotFound,
}
