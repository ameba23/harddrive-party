use async_walkdir::WalkDir;
use std::path::{Path, PathBuf};
// use futures_lite::io::AsyncReadExt;
use futures_lite::stream::StreamExt;

pub const FILES: &[u8; 1] = b"f";
pub const DIRS: &[u8; 1] = b"d";

pub struct Shares {
    files: sled::Tree,
    dirs: sled::Tree,
    // share_names: hashmap str to str
}

impl Shares {
    pub async fn new(storage: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let mut db_dir = storage.as_ref().to_owned();
        db_dir.push("db");
        let db = sled::open(db_dir).expect("open");
        let files = db.open_tree(FILES).unwrap();
        let dirs = db.open_tree(DIRS).unwrap();

        Ok(Shares { files, dirs })
    }

    pub async fn scan(&mut self, root: &str) -> Result<u32, std::io::Error> {
        let mut added_entries = 0;
        let path = PathBuf::from(root);
        let share_name = path.file_name().unwrap();
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
                        let filepath = path.clone().into_os_string().into_string().unwrap();
                        for sub_path in path.ancestors() {
                            println!("sub_path: {:?}", sub_path);
                            // TODO update dirsize
                            // self.dirs.insert(filepath.as_bytes(), b"sfd").unwrap(),
                        }
                        let size = metadata.len().to_le_bytes();
                        // println!("{}", cfg!(target_endian = "big"));
                        self.files.insert(filepath.as_bytes(), &size).unwrap();
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

    pub fn prefix_query(&self, prefix: &str) {
        let prefix_tilde = [prefix, "~"].concat();
        for kv_result in self.files.range(prefix.as_bytes()..prefix_tilde.as_bytes()) {
            let kv = kv_result.unwrap();
            println!("kvr {:?}", std::str::from_utf8(&kv.0).unwrap());
            // TODO convert to pathbuf?
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
        shares.prefix_query("/home/turnip/scrot/2022-10-01");
    }
}
