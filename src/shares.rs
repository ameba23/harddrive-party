use async_walkdir::WalkDir;
use std::path::{Path, PathBuf};
// use futures_lite::io::AsyncReadExt;
use crate::messages::response::ls::Entry;
use futures_lite::stream::StreamExt;
use sled::IVec;
use thiserror::Error;

pub const FILES: &[u8; 1] = b"f";
pub const DIRS: &[u8; 1] = b"d";

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
}

///Error when parsing a Db entry
#[derive(Error, Debug)]
pub enum EntryParseError {
    #[error("Error parsing UTF8")]
    Utf8Error(#[from] std::str::Utf8Error),
    #[error("Error converting database value to u64")]
    U64ConversionError(),
}

fn addition_merge(_key: &[u8], old_value: Option<&[u8]>, merged_bytes: &[u8]) -> Option<Vec<u8>> {
    let old_size = match old_value {
        Some(v) => u64::from_le_bytes(v.try_into().unwrap_or([0; 8])),
        None => 0,
    };
    let to_add = u64::from_le_bytes(merged_bytes.try_into().unwrap_or([0; 8]));
    let new_size = old_size + to_add;
    Some(new_size.to_le_bytes().to_vec())
}

pub struct Shares {
    files: sled::Tree,
    dirs: sled::Tree,
    // share_names: hashmap str to str
}

impl Shares {
    pub async fn new(storage: impl AsRef<Path>) -> Result<Self, CreateSharesError> {
        let mut db_dir = storage.as_ref().to_owned();
        db_dir.push("db");
        let db = sled::open(db_dir).expect("open");
        let files = db.open_tree(FILES)?;
        let dirs = db.open_tree(DIRS)?;
        dirs.set_merge_operator(addition_merge);

        Ok(Shares { files, dirs })
    }

    pub async fn scan(&mut self, root: &str) -> Result<u32, ScanDirError> {
        let mut added_entries = 0;
        let path = PathBuf::from(root);
        let share_name = path.file_name().ok_or_else(|| ScanDirError::GetParentError);
        // self.share_names.insert(share_name, path);
        let mut entries = WalkDir::new(path);
        loop {
            match entries.next().await {
                Some(Ok(entry)) => {
                    let metadata = entry.metadata().await?;
                    if !metadata.is_dir() {
                        // TODO remove the 'path' portion of the entry, and join it with share_name
                        // can use 'strip_prefix'
                        let path = entry.path();
                        let filepath = path
                            .clone()
                            .into_os_string()
                            .into_string()
                            .map_err(|_| ScanDirError::OsStringError())?;
                        let size = metadata.len().to_le_bytes();

                        for sub_path in path.parent().unwrap().ancestors() {
                            println!("sub_path: {:?}", sub_path);
                            // TODO update dirsize
                            let sub_path_bytes = sub_path.to_str().unwrap().as_bytes();
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

    fn kv_to_entry(
        &self,
        (name, size): (IVec, IVec),
        is_dir: bool,
    ) -> Result<Entry, EntryParseError> {
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

    pub fn prefix_query(&self, prefix: &str) {
        for kv_result in self.files.scan_prefix(prefix.as_bytes()) {
            let kv = kv_result.unwrap();
            let entry = self.kv_to_entry(kv, false);
            println!("file {:?}", entry);
            // TODO convert to pathbuf?
        }

        for kv_result in self.dirs.scan_prefix(prefix.as_bytes()) {
            let kv = kv_result.unwrap();
            let entry = self.kv_to_entry(kv, true);
            println!("dir {:?}", entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[async_std::test]
    async fn share() {
        let storage = TempDir::new().unwrap();
        let mut shares = Shares::new(storage).await.unwrap();
        let added = shares.scan("/home/turnip/scrot").await.unwrap();
        println!("added {}", added);
        shares.prefix_query("");
    }
}
