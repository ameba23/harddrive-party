use std::time::{Duration, SystemTime};

use crate::ui_messages::DownloadRequest;
use async_stream::stream;
use futures::{stream::BoxStream, StreamExt};

type KeyValue = (Vec<u8>, Vec<u8>);

/// A requested file

impl DownloadRequest {
    /// Given a wishlist db entry, make a DownloadRequest
    /// key: <peer_public_key><timestamp><path> value: <request_id><size>
    pub fn from_db_key_value(key: Vec<u8>, value: Vec<u8>) -> anyhow::Result<Self> {
        let peer_public_key: [u8; 32] = key[0..32].try_into()?;

        let timestamp_buf: [u8; 8] = key[32..32 + 8].try_into()?;
        let timestamp_secs = u64::from_be_bytes(timestamp_buf);
        let timestamp = Duration::from_secs(timestamp_secs);

        let path = std::str::from_utf8(&key[32 + 8..])?.to_string();

        let request_id_buf: [u8; 4] = value[0..4].try_into()?;
        let request_id = u32::from_be_bytes(request_id_buf);

        let size_buf: [u8; 8] = value[4..4 + 8].try_into()?;
        let size = u64::from_be_bytes(size_buf);

        Ok(Self {
            path,
            size,
            request_id,
            timestamp,
            peer_public_key,
        })
    }

    /// Convert to a db entry for both 'by peer' and 'by timestamp'
    fn to_db_key_value(&self) -> (KeyValue, KeyValue) {
        let path_buf = self.path.as_bytes();
        let mut key: Vec<u8> = Vec::with_capacity(32 + 8 + path_buf.len());
        key.append(&mut self.peer_public_key.clone().to_vec());

        let timestamp_secs = self.timestamp.as_secs();
        let timestamp_secs_buf = timestamp_secs.to_be_bytes();
        key.append(&mut timestamp_secs_buf.to_vec());

        let path_clone = self.path.clone();
        let path_buf = path_clone.as_bytes();
        key.append(&mut path_buf.to_vec());

        let request_id_buf = self.request_id.to_be_bytes();
        let size_buf = self.size.to_be_bytes();

        let mut value: [u8; 4 + 8] = [0u8; 4 + 8];
        value[0..4].copy_from_slice(&request_id_buf);
        value[4..4 + 8].copy_from_slice(&size_buf);
        let kv1 = (key.to_vec(), value.to_vec());

        let mut key2: [u8; 8 + 4] = [0u8; 8 + 4];
        key2[0..8].copy_from_slice(&timestamp_secs_buf);
        key2[8..8 + 4].copy_from_slice(&request_id_buf);

        let kv2 = (key2.to_vec(), self.peer_public_key.clone().to_vec());
        (kv1, kv2)
    }

    // Convert a completed download request to the format for storing a record
    // of the completed download
    fn into_downloaded_db_key_value(self) -> KeyValue {
        // key: <timestamp><request_id><path> value: <peer_public_key><size>
        let path_buf = self.path.as_bytes();
        let mut key: Vec<u8> = Vec::with_capacity(8 + 4 + path_buf.len());

        // Record the time of the completed download
        let system_time = SystemTime::now();
        let timestamp = system_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Time went backwards");

        let timestamp_secs = timestamp.as_secs();
        let timestamp_secs_buf = timestamp_secs.to_be_bytes();
        key.append(&mut timestamp_secs_buf.to_vec());

        let request_id_buf = self.request_id.to_be_bytes();
        key.append(&mut request_id_buf.to_vec());

        key.append(&mut path_buf.to_vec());

        let mut value: [u8; 32 + 8] = [0u8; 32 + 8];
        value[0..32].copy_from_slice(&self.peer_public_key);

        let size_buf = self.size.to_be_bytes();
        value[32..32 + 8].copy_from_slice(&size_buf);

        (key, value.to_vec())
    }

    /// Given a downloaded db record, create a download request
    pub fn from_downloaded_db_key_value(key: Vec<u8>, value: Vec<u8>) -> anyhow::Result<Self> {
        // key: <timestamp><request_id><path> value: <peer_public_key><size>
        let timestamp_buf: [u8; 8] = key[0..8].try_into()?;
        let timestamp_secs = u64::from_be_bytes(timestamp_buf);
        let timestamp = Duration::from_secs(timestamp_secs);

        let request_id_buf: [u8; 4] = key[8..8 + 4].try_into()?;
        let request_id = u32::from_be_bytes(request_id_buf);

        let path = std::str::from_utf8(&key[8 + 4..])?.to_string();

        let peer_public_key: [u8; 32] = value[0..32].try_into()?;

        let size_buf: [u8; 8] = value[32..32 + 8].try_into()?;
        let size = u64::from_be_bytes(size_buf);

        Ok(Self {
            path,
            size,
            request_id,
            timestamp,
            peer_public_key,
        })
    }
}

#[derive(Clone)]
pub struct WishList {
    /// key: <peer_public_key><timestamp><path> value: <request_id><size>
    db_by_peer: sled::Tree,
    /// key: <timestamp><request_id> value: <peer_public_key>
    db_by_timestamp: sled::Tree,
    // key: <timestamp><request_id><path> value: <peer_public_key><size>
    db_downloaded: sled::Tree,
}

impl WishList {
    pub fn new(db: &sled::Db) -> anyhow::Result<Self> {
        Ok(WishList {
            db_by_peer: db.open_tree(b"p")?,
            db_by_timestamp: db.open_tree(b"t")?,
            db_downloaded: db.open_tree(b"D")?,
        })
    }

    /// Get all items to send to UI
    pub fn requested(
        &self,
    ) -> anyhow::Result<Box<dyn Iterator<Item = DownloadRequest> + Send + '_>> {
        let iter = self
            .db_by_timestamp
            .iter()
            .filter_map(|kv_result| match kv_result {
                Ok((key, value)) => {
                    let mut by_peer_key = value.to_vec();
                    by_peer_key.append(&mut key[0..8].to_vec());
                    let iterator =
                        self.db_by_peer
                            .scan_prefix(&by_peer_key)
                            .filter_map(|kv_result| match kv_result {
                                Ok((key, value)) => {
                                    DownloadRequest::from_db_key_value(key.to_vec(), value.to_vec())
                                        .ok()
                                }
                                Err(_) => None,
                            });
                    Some(iterator)
                }
                Err(_) => None,
            })
            .flatten();

        Ok(Box::new(iter))
    }

    pub fn downloaded(
        &self,
    ) -> anyhow::Result<Box<dyn Iterator<Item = DownloadRequest> + Send + '_>> {
        let iter = self
            .db_downloaded
            .iter()
            .filter_map(|kv_result| match kv_result {
                Ok((key, value)) => {
                    DownloadRequest::from_downloaded_db_key_value(key.to_vec(), value.to_vec()).ok()
                }
                Err(_) => None,
            });

        Ok(Box::new(iter))
    }

    // Subscribe to requests for a peer
    pub fn requests_for_peer(
        &self,
        peer_public_key: &[u8; 32],
    ) -> BoxStream<'static, DownloadRequest> {
        let existing_requests =
            self.db_by_peer
                .scan_prefix(peer_public_key)
                .filter_map(|kv_result| match kv_result {
                    Ok((key, value)) => {
                        DownloadRequest::from_db_key_value(key.to_vec(), value.to_vec()).ok()
                    }
                    Err(_) => None,
                });

        let mut subscriber = self.db_by_peer.watch_prefix(peer_public_key);

        let stream = stream! {
            // First add outstanding requests
            for existing_request in existing_requests {
                    yield existing_request;
            }

            // Then add new requests as they arrive
            while let Some(event) = (&mut subscriber).await {
                if let sled::Event::Insert { key, value } = event {
                    let request =
                        DownloadRequest::from_db_key_value(key.to_vec(), value.to_vec()).unwrap();
                        yield request;
                }
            }
        };
        stream.boxed()
    }

    /// Add a download request
    pub fn add(&self, download_request: &DownloadRequest) -> anyhow::Result<()> {
        let ((key, value), (key2, value2)) = download_request.to_db_key_value();
        self.db_by_peer.insert(key, value)?;
        self.db_by_timestamp.insert(key2, value2)?;
        Ok(())
    }

    /// Remove a specific completed item
    pub fn completed(&self, download_request: DownloadRequest) -> anyhow::Result<()> {
        let ((key, _value), (key2, _value2)) = download_request.to_db_key_value();
        self.db_by_peer.remove(key)?;
        self.db_by_timestamp.remove(key2)?;

        // Add the item to 'downloaded' db
        let (key, value) = download_request.into_downloaded_db_key_value();
        self.db_downloaded.insert(key, value)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn create_download_request() {
        let dl_req = DownloadRequest::new(
            "books/book.pdf".to_string(),
            501546,
            5144,
            *b"23lkjfsdfljkfsdlskdjsfdklfsddjsd",
        );
        let ((key, value), (_key2, _value2)) = dl_req.to_db_key_value();
        let decoded_request = DownloadRequest::from_db_key_value(key, value).unwrap();
        assert_eq!(dl_req.path, decoded_request.path);
        assert_eq!(dl_req.size, decoded_request.size);
        assert_eq!(dl_req.request_id, decoded_request.request_id);
        // This will fail if we compare durations due to the missing nanoseconds
        assert_eq!(
            dl_req.timestamp.as_secs(),
            decoded_request.timestamp.as_secs()
        );
        assert_eq!(dl_req.peer_public_key, decoded_request.peer_public_key);
    }

    #[test]
    fn create_wishlist() {
        let storage = TempDir::new().unwrap();
        let mut db_dir = storage.as_ref().to_owned();
        db_dir.push("db");
        let db = sled::open(db_dir).expect("open");
        let wishlist = WishList::new(&db).unwrap();

        let dl_req = DownloadRequest::new(
            "books/book.pdf".to_string(),
            501546,
            5144,
            *b"23lkjfsdfljkfsdlskdjsfdklfsddjsd",
        );

        wishlist.add(&dl_req).unwrap();

        let requested_items = wishlist.requested().unwrap();
        for item in requested_items {
            assert_eq!(dl_req.path, item.path);
            assert_eq!(dl_req.size, item.size);
            assert_eq!(dl_req.request_id, item.request_id);
            // This will fail if we compare durations due to the missing nanoseconds
            assert_eq!(dl_req.timestamp.as_secs(), item.timestamp.as_secs());
            assert_eq!(dl_req.peer_public_key, item.peer_public_key);
        }

        wishlist.completed(dl_req.clone()).unwrap();

        let downloaded_items = wishlist.downloaded().unwrap();

        for item in downloaded_items {
            assert_eq!(dl_req.path, item.path);
            assert_eq!(dl_req.size, item.size);
            assert_eq!(dl_req.request_id, item.request_id);
            // This will fail if we compare durations due to the missing nanoseconds
            assert_eq!(dl_req.timestamp.as_secs(), item.timestamp.as_secs());
            assert_eq!(dl_req.peer_public_key, item.peer_public_key);
        }
    }
}
