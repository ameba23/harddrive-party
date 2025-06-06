//! Index shared directories
use crate::wire_messages::{Entry, LsResponse};
use async_walkdir::WalkDir;
use futures::stream::StreamExt;
use log::{debug, info, warn};
use sled::IVec;
use std::path::{Path, PathBuf, MAIN_SEPARATOR};
use thiserror::Error;

const FILES: &[u8; 1] = b"f";
const DIRS: &[u8; 1] = b"d";
const SHARE_NAMES: &[u8; 1] = b"s";

/// The maximum number of query results we can store in a single message
pub const MAX_ENTRIES_PER_MESSAGE: usize = 64;

/// The share index
#[derive(Clone)]
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
    pub async fn new(db: sled::Db, share_dirs: Vec<String>) -> Result<Self, CreateSharesError> {
        let files = db.open_tree(FILES)?;
        let dirs = db.open_tree(DIRS)?;
        dirs.set_merge_operator(addition_merge);
        let share_names = db.open_tree(SHARE_NAMES)?;

        let mut shares = Shares {
            files,
            dirs,
            share_names,
        };

        for share_dir in share_dirs {
            shares.scan(&share_dir).await?;
        }

        Ok(shares)
    }

    /// Index a given directory and return the number of entries added to the database
    // TODO #16 handle share name collisions
    pub async fn scan(&mut self, root: &str) -> Result<u32, ScanDirError> {
        let mut added_entries = 0;
        let path = PathBuf::from(root);
        let path_clone = &path.clone();

        // share_name is what we refer to the shared dir by
        let path_clone_2 = path.clone();
        let share_name = path_clone_2
            .file_name()
            .ok_or(ScanDirError::GetParentError)?
            .to_str()
            .ok_or(ScanDirError::OsStringError())?;

        let path_os_str = path.clone().into_os_string();
        let path_str = path_os_str.to_str().ok_or(ScanDirError::OsStringError())?;
        self.share_names.insert(share_name, path_str)?;

        // Remove existing entries before beginning
        self.remove_share_dir(share_name)?;

        let mut entries = WalkDir::new(path);
        loop {
            match entries.next().await {
                Some(Ok(entry)) => {
                    let metadata = entry.metadata().await?;
                    if !metadata.is_dir() {
                        // Remove the 'path' portion of the entry, and join it with share_name
                        let ep = entry.path();
                        let entry_path = ep.strip_prefix(path_clone)?;
                        let sn = path_clone.file_name().ok_or(ScanDirError::GetParentError)?;
                        let entry_path_with_share_name = Path::new(sn).join(entry_path);
                        let filepath = entry_path_with_share_name
                            .to_str()
                            .ok_or(ScanDirError::OsStringError())?;

                        let size = metadata.len().to_le_bytes();

                        // For each component of the path, add the size into the directory sizes index
                        for sub_path in entry_path_with_share_name
                            .parent()
                            .ok_or(ScanDirError::GetParentError)?
                            .ancestors()
                        {
                            let sub_path_bytes = sub_path
                                .to_str()
                                .ok_or(ScanDirError::OsStringError())?
                                .as_bytes();
                            self.dirs.merge(sub_path_bytes, size)?;
                        }
                        self.files.insert(filepath.as_bytes(), &size)?;
                        info!("{:?} {:?}", entry.path(), entry.metadata().await?.is_file());
                        added_entries += 1;
                    }
                }
                Some(Err(e)) => {
                    warn!("Error {}", e);
                    return Err(ScanDirError::IOError(e));
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
        searchterm: Option<String>,
        recursive: bool,
    ) -> Result<Box<dyn Iterator<Item = LsResponse> + Send>, EntryParseError> {
        let path = path_option.unwrap_or_default();

        // Check that the given subdir / file exists
        if let Ok(None) = self.dirs.get(&path) {
            if let Ok(None) = self.files.get(&path) {
                return Err(EntryParseError::PathNotFound);
            }
        }

        let path_len = path.len();
        let searchterm_clone = searchterm.clone();

        let dirs_iter = self.dirs.scan_prefix(&path).filter_map(move |kv_result| {
            kv_filter_map(kv_result, true, recursive, path_len, &searchterm)
        });

        let files_iter = self.files.scan_prefix(&path).filter_map(move |kv_result| {
            kv_filter_map(kv_result, false, recursive, path_len, &searchterm_clone)
        });

        let entries_iter = dirs_iter.chain(files_iter);

        let chunked = Chunker {
            inner: Box::new(entries_iter),
            chunk_size: MAX_ENTRIES_PER_MESSAGE,
        };

        let response_iter = chunked.map(LsResponse::Success);

        Ok(Box::new(response_iter))
    }

    /// Resolve a path from a request by looking up the absolute path associated with its share name
    /// component
    pub fn resolve_path(&self, input_path: String) -> Result<(PathBuf, u64), ResolvePathError> {
        info!("Resolving path {}", input_path);

        let size = match self.files.get(&input_path)? {
            Some(size_buf) => u64::from_le_bytes(
                size_buf
                    .to_vec()
                    .try_into()
                    .map_err(|_| ResolvePathError::BadShareName)?,
            ),
            None => {
                return Err(ResolvePathError::FileNotFound);
            }
        };

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
        Ok((actual_path.join(sub_path), size))
    }

    /// Stop sharing a directory by removing related entries from the database
    pub fn remove_share_dir(&mut self, share_name: &str) -> Result<(), ScanDirError> {
        // First find the old total size of share dir and subtract it from the "" entry
        if let Some(existing_size) = self.get_dir_size(share_name) {
            self.dirs
                .fetch_and_update("", |root_size_option: Option<&[u8]>| {
                    let new_size = match root_size_option {
                        Some(root_size_buf) => match root_size_buf.to_vec().try_into() {
                            Ok(root_size_arr) => {
                                let root_size = u64::from_le_bytes(root_size_arr);
                                root_size - existing_size
                            }
                            Err(_) => 0,
                        },
                        None => 0,
                    };
                    Some(new_size.to_le_bytes().to_vec())
                })?;
        }

        for (entry, _) in self.dirs.scan_prefix(share_name).flatten() {
            debug!("Deleting existing entry {:?}", entry);
            self.dirs.remove(entry)?;
        }
        for (entry, _) in self.files.scan_prefix(share_name).flatten() {
            debug!("Deleting existing entry {:?}", entry);
            self.files.remove(entry)?;
        }
        Ok(())
    }

    fn get_dir_size(&mut self, dir_name: &str) -> Option<u64> {
        let existing_ivec = self.dirs.get(dir_name).ok()??;
        Some(u64::from_le_bytes(existing_ivec.to_vec().try_into().ok()?))
    }
}

/// Filter a key/value database entry based on query and if selected convert to a struct
fn kv_filter_map(
    kv_result: Result<(IVec, IVec), sled::Error>,
    is_dir: bool,
    recursive: bool,
    path_len: usize,
    searchterm: &Option<String>,
) -> Option<Entry> {
    let (name, size) = kv_result.ok()?;
    let name = std::str::from_utf8(&name).ok()?;

    if !recursive {
        // TODO should we use pathbuf for this?
        let full_suffix = &name[path_len..];
        let suffix = if full_suffix.starts_with(MAIN_SEPARATOR) {
            &full_suffix[1..]
        } else {
            full_suffix
        };
        if suffix.contains(MAIN_SEPARATOR) {
            return None;
        }
    }

    if let Some(search) = searchterm {
        if !name.contains(search) {
            return None;
        };
    }

    let size = u64::from_le_bytes(size.to_vec().try_into().ok()?);

    Some(Entry {
        name: name.to_string(),
        size,
        is_dir,
    })
}

/// Turn an iterator into an iterator containing vectors of chunks of a given size
pub struct Chunker<T> {
    pub inner: Box<dyn Iterator<Item = T> + Send>,
    pub chunk_size: usize,
}

impl<T> Iterator for Chunker<T> {
    type Item = Vec<T>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut entries = Vec::new();
        for e in self.inner.by_ref() {
            entries.push(e);
            if entries.len() == self.chunk_size {
                return Some(entries);
            }
        }
        match entries.len() {
            0 => None,
            _ => Some(entries),
        }
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
    #[error(transparent)]
    ScanDirError(#[from] ScanDirError),
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
    #[error("Error converting database value to u64")]
    U64ConversionError,
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
    #[error("Path not found")]
    PathNotFound,
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
    #[error("File does not exist in db")]
    FileNotFound,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_entries() -> Vec<Entry> {
        vec![
            Entry {
                name: "".to_string(),
                size: 17,
                is_dir: true,
            },
            Entry {
                name: "test-data".to_string(),
                size: 17,
                is_dir: true,
            },
            Entry {
                name: "test-data/subdir".to_string(),
                size: 12,
                is_dir: true,
            },
            Entry {
                name: "test-data/subdir/subsubdir".to_string(),
                size: 6,
                is_dir: true,
            },
            Entry {
                name: "test-data/somefile".to_string(),
                size: 5,
                is_dir: false,
            },
            Entry {
                name: "test-data/subdir/anotherfile".to_string(),
                size: 6,
                is_dir: false,
            },
            Entry {
                name: "test-data/subdir/subsubdir/yetanotherfile".to_string(),
                size: 6,
                is_dir: false,
            },
        ]
    }

    #[tokio::test]
    async fn share_query() {
        let storage = TempDir::new().unwrap();
        let mut db_dir = storage.as_ref().to_owned();
        db_dir.push("db");
        let db = sled::open(db_dir).expect("open");

        let mut shares = Shares::new(db.clone(), Vec::new()).await.unwrap();
        let added = shares.scan("tests/test-data").await.unwrap();
        assert_eq!(added, 3);

        let mut test_entries = create_test_entries();
        let responses = shares.query(None, None, true).unwrap();
        for res in responses {
            match res {
                LsResponse::Success(entries) => {
                    for entry in entries {
                        let i = test_entries.iter().position(|e| e == &entry).unwrap();
                        test_entries.remove(i);
                    }
                }
                LsResponse::Err(err) => {
                    panic!("Got error response {:?}", err);
                }
            }
        }
        // Make sure we found every entry
        assert_eq!(test_entries.len(), 0);

        // Try resolving a path name
        let (resolved, _size) = shares
            .resolve_path("test-data/subdir/anotherfile".to_string())
            .unwrap();
        assert_eq!(
            resolved,
            PathBuf::from("tests/test-data/subdir/anotherfile")
        );

        // Repeat the process with a new shares instance using the same db, to simulate restarting
        // the program
        let mut shares_2 = Shares::new(db, Vec::new()).await.unwrap();

        let added = shares_2.scan("tests/test-data").await.unwrap();
        assert_eq!(added, 3);

        let mut test_entries = create_test_entries();
        let responses = shares_2.query(None, None, true).unwrap();
        for res in responses {
            match res {
                LsResponse::Success(entries) => {
                    for entry in entries {
                        let i = test_entries.iter().position(|e| e == &entry).unwrap();
                        test_entries.remove(i);
                    }
                }
                LsResponse::Err(err) => {
                    panic!("Got error response {:?}", err);
                }
            }
        }
        // Make sure we found every entry
        assert_eq!(test_entries.len(), 0);
    }
}
