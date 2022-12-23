use crate::messages::response;
use crate::messages::response::ls::Entry;
use crate::messages::response::{Response, Success};
use async_walkdir::WalkDir;
use futures_lite::stream::StreamExt;
use futures_lite::Stream;
use sled::IVec;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const FILES: &[u8; 1] = b"f";
pub const DIRS: &[u8; 1] = b"d";
pub const SHARE_NAMES: &[u8; 1] = b"s";

// This will be a higher value - but keeping it small to test chunking
pub const MAX_ENTRIES_PER_MESSAGE: usize = 3;

/// The share index
pub struct Shares {
    /// Filepaths mapped to their size in bytes
    files: sled::Tree,
    /// Directory paths mapped to their size in bytes
    dirs: sled::Tree,
    /// The displayed names of shared directories mapped to their actual path on disk
    share_names: sled::Tree, // or should this be an in-memory hashmap?
}

impl Shares {
    /// Setup share index giving a path to use for persistant storage
    pub async fn new(storage: impl AsRef<Path>) -> Result<Self, CreateSharesError> {
        let mut db_dir = storage.as_ref().to_owned();
        db_dir.push("db");
        let db = sled::open(db_dir).expect("open");
        let files = db.open_tree(FILES)?;
        let dirs = db.open_tree(DIRS)?;
        dirs.set_merge_operator(addition_merge);
        let share_names = db.open_tree(SHARE_NAMES)?;

        Ok(Shares {
            files,
            dirs,
            share_names,
        })
    }

    /// Index a given directory and return the number of entries added to the database
    pub async fn scan(&mut self, root: &str) -> Result<u32, ScanDirError> {
        let mut added_entries = 0;
        let path = PathBuf::from(root);
        let pc = &path.clone();

        let path_clone = path.clone();
        let share_name = path_clone
            .file_name()
            .ok_or_else(|| ScanDirError::GetParentError)?
            .to_str()
            .ok_or_else(|| ScanDirError::OsStringError())?;

        let path_os_str = path.clone().into_os_string();
        let path_str = path_os_str
            .to_str()
            .ok_or_else(|| ScanDirError::OsStringError())?;
        self.share_names.insert(share_name, path_str)?;

        let mut entries = WalkDir::new(path);
        loop {
            match entries.next().await {
                Some(Ok(entry)) => {
                    let metadata = entry.metadata().await?;
                    if !metadata.is_dir() {
                        // Remove the 'path' portion of the entry, and join it with share_name
                        let ep = entry.path();
                        let entry_path = ep.strip_prefix(pc)?;
                        let sn = pc.file_name().ok_or_else(|| ScanDirError::GetParentError)?;
                        let entry_path_with_share_name = Path::new(sn).join(entry_path);
                        let filepath = entry_path_with_share_name
                            .to_str()
                            .ok_or_else(|| ScanDirError::OsStringError())?;

                        let size = metadata.len().to_le_bytes();

                        // For each component of the path, add the size into the directory sizes index
                        for sub_path in entry_path_with_share_name
                            .parent()
                            .ok_or_else(|| ScanDirError::GetParentError)?
                            .ancestors()
                        {
                            let sub_path_bytes = sub_path
                                .to_str()
                                .ok_or_else(|| ScanDirError::OsStringError())?
                                .as_bytes();
                            self.dirs.merge(sub_path_bytes, size)?;
                        }

                        self.files.insert(filepath.as_bytes(), &size)?;
                        println!("{:?} {:?}", entry.path(), entry.metadata().await?.is_file());
                        added_entries += 1;
                    }
                }
                Some(Err(e)) => {
                    eprintln!("Error {}", e);
                    break;
                }
                None => break,
            };
        }
        Ok(added_entries)
    }

    /// ls or search query
    pub fn query(
        &self,
        path_option: Option<String>,
        _searchterm: Option<String>,
        _recursive: Option<bool>,
    ) -> Result<Box<dyn Stream<Item = Response> + Send + '_>, EntryParseError> {
        let path = path_option.unwrap_or("".to_string());
        let dirs_iter = self
            .dirs
            .scan_prefix(&path)
            .map_while(|kv_result| kv_to_entry(kv_result, true).ok());

        let files_iter = self
            .files
            .scan_prefix(&path)
            .map_while(|kv_result| kv_to_entry(kv_result, false).ok());

        let entries_iter = dirs_iter.chain(files_iter);

        let chunked = Chunker {
            inner: Box::new(entries_iter),
            chunk_size: MAX_ENTRIES_PER_MESSAGE,
        };

        let response_iter = chunked.map(|entries| {
            let response = Response::Success(Success {
                msg: Some(response::success::Msg::Ls(response::Ls { entries })),
            });
            response
        });
        let is = futures_lite::stream::iter(response_iter);
        Ok(Box::new(is))
    }

    /// Resolve a path from a request by looking up the absolute path associated with its share name
    /// component
    /// Note this currently does not check if the file exists in the db or on disk
    pub fn resolve_path(&self, input_path: String) -> Result<PathBuf, ResolvePathError> {
        let input_path_path_buf = PathBuf::from(input_path);
        let mut input_path_iter = input_path_path_buf.iter();
        let share_name = input_path_iter
            .next()
            .ok_or(ResolvePathError::MissingFirstComponent)?;

        let sub_path: PathBuf = input_path_iter.collect();

        let share_name_bytes = share_name
            .to_str()
            .ok_or(ResolvePathError::MissingFirstComponent)?
            .as_bytes();

        let actual_path_bytes = self
            .share_names
            .get(share_name_bytes)?
            .ok_or(ResolvePathError::BadShareName)?;

        let actual_path = PathBuf::from(std::str::from_utf8(&actual_path_bytes)?);
        Ok(actual_path.join(sub_path))
    }
}

/// Convert a key/value database entry into a struct
fn kv_to_entry(
    kv_result: Result<(IVec, IVec), sled::Error>,
    is_dir: bool,
) -> Result<Entry, EntryParseError> {
    let (name, size) = kv_result?;
    let name = std::str::from_utf8(&name)?;
    let size = u64::from_le_bytes(
        size.to_vec()
            .try_into()
            .map_err(|_| EntryParseError::U64ConversionError())?,
    );
    Ok(Entry {
        name: name.to_string(),
        size,
        is_dir,
    })
}

/// Turn an iterator into an iterator containing vectors of chunks of a given size
struct Chunker {
    inner: Box<dyn Iterator<Item = Entry> + Send>,
    chunk_size: usize,
}

impl Iterator for Chunker {
    type Item = Vec<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut entries = Vec::new();
        while let Some(e) = self.inner.next() {
            entries.push(e);
            if entries.len() == self.chunk_size {
                return Some(entries);
            }
        }
        None
    }
}

/// To make cumulative directory sizes by adding the size of their containing files
fn addition_merge(_key: &[u8], old_value: Option<&[u8]>, merged_bytes: &[u8]) -> Option<Vec<u8>> {
    let old_size = match old_value {
        Some(v) => u64::from_le_bytes(v.try_into().unwrap_or([0; 8])),
        None => 0,
    };
    let to_add = u64::from_le_bytes(merged_bytes.try_into().unwrap_or([0; 8]));
    let new_size = old_size + to_add;
    Some(new_size.to_le_bytes().to_vec())
}

/// Error when creating a Shares struct
#[derive(Error, Debug)]
pub enum CreateSharesError {
    #[error(transparent)]
    IOError(#[from] sled::Error),
}

/// Error when indexing a dir
#[derive(Error, Debug)]
pub enum ScanDirError {
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    #[error("Cannot parse OsString")]
    OsStringError(),
    #[error("Unable to merge db record")]
    DbMergeError(#[from] sled::Error),
    #[error("Cannot get parent of given dir")]
    GetParentError,
    #[error("Got entry which does not appear to be a child of the given directory")]
    PrefixError(#[from] std::path::StripPrefixError),
}

/// Error when parsing a Db entry
#[derive(Error, Debug)]
pub enum EntryParseError {
    #[error("Db error")]
    DbError(#[from] sled::Error),
    #[error("Error parsing UTF8")]
    Utf8Error(#[from] std::str::Utf8Error),
    #[error("Error converting database value to u64")]
    U64ConversionError(),
}

/// Error when resolving a path from a request
#[derive(Error, Debug)]
pub enum ResolvePathError {
    #[error("Db error")]
    DbError(#[from] sled::Error),
    #[error("Cannot get share name")]
    MissingFirstComponent,
    #[error("Cannot find share name in db")]
    BadShareName,
    #[error("Error parsing UTF8")]
    Utf8Error(#[from] std::str::Utf8Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_std::task;
    use tempfile::TempDir;

    // TODO use standard test directory

    #[async_std::test]
    async fn share_query() {
        let storage = TempDir::new().unwrap();
        let mut shares = Shares::new(storage).await.unwrap();
        let added = shares.scan("/home/turnip/Hipax").await.unwrap();
        println!("added {}", added);

        let mut responses = Box::into_pin(shares.query(None, None, None).unwrap());
        while let Some(res) = responses.next().await {
            println!("Shareq {:?}", res);
        }

        let resolved = shares
            .resolve_path("Hipax/df/aslkjdsal.asds".to_string())
            .unwrap();
        assert_eq!(
            resolved,
            PathBuf::from("/home/turnip/Hipax/df/aslkjdsal.asds")
        );
    }

    #[async_std::test]
    async fn query_from_thread() {
        let storage = TempDir::new().unwrap();
        let mut shares = Shares::new(storage).await.unwrap();
        shares.scan("/home/turnip/Hipax").await.unwrap();
        task::spawn(async move {
            let mut responses = Box::into_pin(shares.query(None, None, None).unwrap());
            while let Some(res) = responses.next().await {
                println!("Shareq {:?}", res);
            }
        });
    }
}
