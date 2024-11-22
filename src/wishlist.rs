//! Track requested and downloaded files
use crate::{
    hdp::{
        COMPLETED_REQUESTS, DOWNLOADED, REQUESTED_FILES_BY_PEER, REQUESTS, REQUESTS_BY_TIMESTAMP,
        REQUESTS_PROGRESS,
    },
    ui_messages::{UiDownloadRequest, UiServerMessage},
};
use anyhow::anyhow;
use async_stream::stream;
use futures::{stream::BoxStream, StreamExt};
use harddrive_party_shared::ui_messages::UiRequestedFile;
use key_to_animal::key_to_name;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc::Sender;

/// How many entries to send when updating the UI
const MAX_ENTRIES: usize = 100;

type KeyValue = (Vec<u8>, Vec<u8>);

/// A requested file or directory, with a unique request id
/// If this is a directory, it may be composed of more than one [RequestedFile]
#[derive(PartialEq, Debug, Clone)]
pub struct DownloadRequest {
    /// The path of the file or directory on the remote
    pub path: String,
    /// The total size in bytes
    pub total_size: u64,
    /// Identifier for this request
    pub request_id: u32,
    /// Time when request made relative to unix epoch
    pub timestamp: Duration,
    /// Public key of peer holding file
    pub peer_public_key: [u8; 32],
}

impl DownloadRequest {
    pub fn new(path: String, total_size: u64, request_id: u32, peer_public_key: [u8; 32]) -> Self {
        let system_time = SystemTime::now();
        let timestamp = system_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Time went backwards");
        Self {
            path,
            total_size,
            request_id,
            timestamp,
            peer_public_key,
        }
    }

    /// Given a serialized db entry, make a DownloadRequest
    /// key: `<request_id>` value: `<timestamp><total_size><peer_public_key><requested_path>`
    pub fn from_db_key_value(key: Vec<u8>, value: Vec<u8>) -> anyhow::Result<Self> {
        if key.len() != 4 {
            return Err(anyhow!("Unexpected key length (should be 4 bytes)"));
        }
        if value.len() < 8 + 8 + 32 {
            return Err(anyhow!(
                "Unexpected value length (should be at least 48 bytes)"
            ));
        }
        let request_id_buf: [u8; 4] = key[0..4].try_into()?;
        let request_id = u32::from_be_bytes(request_id_buf);

        let timestamp_buf: [u8; 8] = value[0..8].try_into()?;
        let timestamp_secs = u64::from_be_bytes(timestamp_buf);
        let timestamp = Duration::from_secs(timestamp_secs);

        let total_size_buf: [u8; 8] = value[8..16].try_into()?;
        let total_size = u64::from_be_bytes(total_size_buf);

        let peer_public_key: [u8; 32] = value[16..48].try_into()?;

        let path = std::str::from_utf8(&value[48..])?.to_string();

        Ok(Self {
            path,
            total_size,
            request_id,
            timestamp,
            peer_public_key,
        })
    }

    /// Convert to a db entry for both 'by request_id' and 'by timestamp'
    ///
    /// key: `<request_id>` value: `<timestamp><total_size><peer_public_key><requested_path>`
    /// key: <timestamp> value: `<request_id>`
    fn to_db_key_value(&self) -> (KeyValue, KeyValue) {
        let path_buf = self.path.as_bytes();
        // let mut key: Vec<u8> = Vec::with_capacity(32 + 8 + path_buf.len());
        // key.append(&mut self.peer_public_key.clone().to_vec());

        let timestamp_secs = self.timestamp.as_secs();
        let timestamp_secs_buf = timestamp_secs.to_be_bytes();
        // key.append(&mut timestamp_secs_buf.to_vec());
        //
        // let path_clone = self.path.clone();
        // let path_buf = path_clone.as_bytes();
        // key.append(&mut path_buf.to_vec());
        //
        let request_id_buf = self.request_id.to_be_bytes();
        let total_size_buf = self.total_size.to_be_bytes();

        let mut value1 = Vec::with_capacity(8 + 8 + 32 + path_buf.len());
        value1.append(&mut timestamp_secs_buf.to_vec());
        value1.append(&mut total_size_buf.to_vec());
        value1.append(&mut self.peer_public_key.clone().to_vec());
        value1.append(&mut path_buf.to_vec());
        let kv1 = (request_id_buf.to_vec(), value1);

        let kv2 = (timestamp_secs_buf.to_vec(), request_id_buf.to_vec());
        //
        // let mut value: [u8; 4 + 8] = [0u8; 4 + 8];
        // value[0..4].copy_from_slice(&request_id_buf);
        // value[4..4 + 8].copy_from_slice(&size_buf);
        // let kv1 = (key.to_vec(), value.to_vec());
        //
        // let mut key2: [u8; 8 + 4] = [0u8; 8 + 4];
        // key2[0..8].copy_from_slice(&timestamp_secs_buf);
        // key2[8..8 + 4].copy_from_slice(&request_id_buf);
        //
        // let requested_path = requested_path.unwrap_or_default();
        // let mut value2: Vec<u8> = Vec::with_capacity(32 + requested_path.len());
        // value2.append(&mut self.peer_public_key.to_vec());
        // value2.append(&mut requested_path.as_bytes().to_vec());
        //
        // let kv2 = (key2.to_vec(), value2);
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

        let size_buf = self.total_size.to_be_bytes();
        value[32..32 + 8].copy_from_slice(&size_buf);

        (key, value.to_vec())
    }

    /// Given a completed requests db record, create a download request
    ///
    /// key: `<request_id>` value: `<timestamp><peer_public_key><total_size><path>`
    pub fn from_completed_requests_db_key_value(
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> anyhow::Result<Self> {
        let request_id_buf: [u8; 4] = key[..4].try_into()?;
        let request_id = u32::from_be_bytes(request_id_buf);

        let timestamp_buf: [u8; 8] = value[0..8].try_into()?;
        let timestamp_secs = u64::from_be_bytes(timestamp_buf);
        let timestamp = Duration::from_secs(timestamp_secs);

        let peer_public_key: [u8; 32] = value[8..32 + 8].try_into()?;

        let total_size_buf: [u8; 8] = value[32 + 8..32 + 8 + 8].try_into()?;
        let total_size = u64::from_be_bytes(total_size_buf);

        let path = std::str::from_utf8(&key[32 + 8 + 8..])?.to_string();

        Ok(Self {
            path,
            total_size,
            request_id,
            timestamp,
            peer_public_key,
        })
    }

    pub fn into_ui_download_request(self) -> UiDownloadRequest {
        UiDownloadRequest {
            path: self.path,
            total_size: self.total_size,
            request_id: self.request_id,
            timestamp: self.timestamp,
            peer_name: key_to_name(&self.peer_public_key),
        }
    }
}

/// A requested file
#[derive(PartialEq, Debug, Clone)]
pub struct RequestedFile {
    /// The path of the file on the remote
    pub path: String,
    /// The size in bytes
    pub size: u64,
    /// This id is not unique - it references which request this came from
    /// requesting a directory will be split into requests for each file
    pub request_id: u32,
}

impl RequestedFile {
    /// Given a serialized wishlist db entry, make a [RequestedFile]
    /// key: `<peer_public_key><timestamp><path>` value: `<request_id><size>`
    fn from_db_key_value(key: Vec<u8>, value: Vec<u8>) -> anyhow::Result<Self> {
        // TODO would it be useful to also return these?
        // let peer_public_key: [u8; 32] = key[0..32].try_into()?;

        // let timestamp_buf: [u8; 8] = key[32..32 + 8].try_into()?;
        // let timestamp_secs = u64::from_be_bytes(timestamp_buf);
        // let timestamp = Duration::from_secs(timestamp_secs);

        let path = std::str::from_utf8(&key[32 + 8..])?.to_string();

        let request_id_buf: [u8; 4] = value[0..4].try_into()?;
        let request_id = u32::from_be_bytes(request_id_buf);

        let size_buf: [u8; 8] = value[4..4 + 8].try_into()?;
        let size = u64::from_be_bytes(size_buf);

        Ok(Self {
            path,
            size,
            request_id,
        })
    }

    /// Convert to a db entry for both 'by peer' and 'by timestamp'
    fn to_db_key_value(
        &self,
        peer_public_key: [u8; 32],
        timestamp: Duration,
    ) -> (KeyValue, KeyValue) {
        let path_buf = self.path.as_bytes();
        let mut key: Vec<u8> = Vec::with_capacity(32 + 8 + path_buf.len());
        key.append(&mut peer_public_key.clone().to_vec());

        let timestamp_secs = timestamp.as_secs();
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

        // let requested_path = requested_path.unwrap_or_default();
        // let mut value2: Vec<u8> = Vec::with_capacity(32 + requested_path.len());
        // value2.append(&mut self.peer_public_key.to_vec());
        // value2.append(&mut requested_path.as_bytes().to_vec());

        let kv2 = (key2.to_vec(), peer_public_key.to_vec());
        (kv1, kv2)
    }

    /// key: `<timestamp><request_id><path>` value: `<size>`
    fn from_downloaded_files_db_key_value(key: Vec<u8>, value: Vec<u8>) -> anyhow::Result<Self> {
        // TODO add assertions that key / value have minimum length
        let request_id_buf: [u8; 4] = key[8..8 + 4].try_into()?;
        let request_id = u32::from_be_bytes(request_id_buf);

        let path = std::str::from_utf8(&key[8 + 4..])?.to_string();

        let size_buf: [u8; 8] = value[..8].try_into()?;
        let size = u64::from_be_bytes(size_buf);
        Ok(Self {
            path,
            size,
            request_id,
        })
    }

    fn into_ui_requested_file(self) -> UiRequestedFile {
        UiRequestedFile {
            path: self.path,
            size: self.size,
            request_id: self.request_id,
        }
    }
}

/// Keeps a record of requested and downloaded files
#[derive(Clone)]
pub struct WishList {
    /// Requests
    /// key: `<request_id>` value: `<timestamp><total_size><peer_public_key><requested_path>`
    requests_by_request_id: sled::Tree,
    /// Requests by timestamp
    /// key: `<timestamp>` value: `<request_id>`
    requests_by_timestamp: sled::Tree,
    /// Download progress for a particular request.
    /// This is only updated once per file
    /// key: `<request_id>` value: `<bytes_downloaded>`
    bytes_downloaded_by_request_id: sled::Tree,
    /// Requested files ordered by peer
    /// This is used by the peer handler to subscribe to incoming requests
    /// key: `<peer_public_key><timestamp><path>` value: `<request_id><size>`
    requested_files_by_peer: sled::Tree,
    /// Downloaded files
    /// key: `<timestamp><request_id><path>` value: `<size>`
    db_downloaded_files: sled::Tree,
    /// Completed download requests
    /// key: `<request_id>` value: `<timestamp><peer_public_key><total_size><path>`
    db_completed_requests: sled::Tree,
    response_tx: Sender<UiServerMessage>,
}

impl WishList {
    pub fn new(db: &sled::Db, response_tx: Sender<UiServerMessage>) -> anyhow::Result<Self> {
        Ok(WishList {
            requests_by_request_id: db.open_tree(REQUESTS)?,
            requests_by_timestamp: db.open_tree(REQUESTS_BY_TIMESTAMP)?,
            requested_files_by_peer: db.open_tree(REQUESTED_FILES_BY_PEER)?,
            bytes_downloaded_by_request_id: db.open_tree(REQUESTS_PROGRESS)?,
            db_downloaded_files: db.open_tree(DOWNLOADED)?,
            db_completed_requests: db.open_tree(COMPLETED_REQUESTS)?,
            response_tx,
        })
    }

    /// Get all requested items to send to UI
    pub fn requested(
        &self,
    ) -> anyhow::Result<Box<dyn Iterator<Item = UiDownloadRequest> + Send + '_>> {
        let iter = self.requests_by_timestamp.iter().filter_map(|kv_result| {
            let (_key, request_id) = kv_result.ok()?;
            let request_details = self.requests_by_request_id.get(&request_id).ok()??;
            let download_request =
                DownloadRequest::from_db_key_value(request_id.to_vec(), request_details.to_vec())
                    .ok()?;
            Some(download_request.into_ui_download_request())
        });

        Ok(Box::new(iter))
    }

    /// Get all downloaded items to send to UI
    pub fn downloaded(
        &self,
    ) -> anyhow::Result<Box<dyn Iterator<Item = UiRequestedFile> + Send + '_>> {
        let iter = self
            .db_downloaded_files
            .iter()
            .filter_map(|kv_result| match kv_result {
                Ok((key, value)) => {
                    match RequestedFile::from_downloaded_files_db_key_value(
                        key.to_vec(),
                        value.to_vec(),
                    ) {
                        Ok(download_request) => Some(download_request.into_ui_requested_file()),
                        Err(_) => None,
                    }
                }
                Err(_) => None,
            });

        Ok(Box::new(iter))
    }

    /// Subscribe to requested files for a particular peer
    pub fn requests_for_peer(
        &self,
        peer_public_key: &[u8; 32],
    ) -> BoxStream<'static, RequestedFile> {
        let existing_requests = self
            .requested_files_by_peer
            .scan_prefix(peer_public_key)
            .filter_map(|kv_result| match kv_result {
                Ok((key, value)) => {
                    RequestedFile::from_db_key_value(key.to_vec(), value.to_vec()).ok()
                }
                Err(_) => None,
            });

        let mut subscriber = self.requested_files_by_peer.watch_prefix(peer_public_key);

        let stream = stream! {
            // First add outstanding requests
            for existing_request in existing_requests {
                    yield existing_request;
            }

            // Then add new requests as they arrive
            while let Some(event) = (&mut subscriber).await {
                if let sled::Event::Insert { key, value } = event {
                    let request =
                        RequestedFile::from_db_key_value(key.to_vec(), value.to_vec()).unwrap();
                        yield request;
                }
            }
        };
        stream.boxed()
    }

    /// Add a download request
    pub fn add_request(&self, download_request: &DownloadRequest) -> anyhow::Result<()> {
        let ((key, value), (key2, value2)) = download_request.to_db_key_value();
        self.requests_by_request_id.insert(key, value)?;
        self.requests_by_timestamp.insert(key2, value2)?;
        // TODO here we should send a download response indicating that a request is made
        // or do it in the fn that calls this
        Ok(())
    }

    /// Add a download request for a particular file
    pub fn add_requested_file(&self, requested_file: &RequestedFile) -> anyhow::Result<()> {
        let download_request = self.get_request(requested_file.request_id)?;

        let ((key, value), (key2, value2)) = requested_file
            .to_db_key_value(download_request.peer_public_key, download_request.timestamp);
        self.requested_files_by_peer.insert(key, value)?;
        self.requests_by_timestamp.insert(key2, value2)?;

        Ok(())
    }

    /// Remove a specific completed item from the wishlist and
    /// add it to the downloaded list
    pub fn file_completed(&self, requested_file: RequestedFile) -> anyhow::Result<()> {
        let request_id = requested_file.request_id.to_be_bytes();
        let request_details = self
            .requests_by_request_id
            .get(request_id)?
            .ok_or(anyhow!("Unknown request ID"))?;
        let download_request =
            DownloadRequest::from_db_key_value(request_id.to_vec(), request_details.to_vec())?;
        let ((key, _value), (_key2, _value2)) = requested_file
            .to_db_key_value(download_request.peer_public_key, download_request.timestamp);
        self.requested_files_by_peer.remove(key)?;

        // Update download progress
        let exisitng_bytes = self.get_download_progress_for_request(requested_file.request_id)?;
        let updated_bytes = exisitng_bytes + requested_file.size;
        self.bytes_downloaded_by_request_id
            .insert(request_id, updated_bytes.to_be_bytes().to_vec())?;

        // Add the item to 'downloaded' db
        // TODO
        // let (key, value) = requested_file.into_downloaded_db_key_value();
        // self.db_downloaded.insert(key, value)?;
        // self.updated().await?;

        // TODO if this was the last file associatd with this download request, inform the UI
        Ok(())
    }

    pub async fn requested_files(&self) -> anyhow::Result<Vec<UiDownloadRequest>> {
        let requested: Vec<UiDownloadRequest> = self.requested()?.take(MAX_ENTRIES).collect();
        Ok(requested)
    }

    pub async fn downloaded_files(&self) -> anyhow::Result<Vec<UiRequestedFile>> {
        let downloaded: Vec<UiRequestedFile> = self.downloaded()?.take(MAX_ENTRIES).collect();
        Ok(downloaded)
    }

    pub fn get_download_progress_for_request(&self, request_id: u32) -> anyhow::Result<u64> {
        Ok(
            match self
                .bytes_downloaded_by_request_id
                .get(request_id.to_be_bytes())?
            {
                Some(existing_bytes_buf) => u64::from_be_bytes(
                    existing_bytes_buf
                        .to_vec()
                        .try_into()
                        .map_err(|_| anyhow!("Bad existing bytes entry"))?,
                ),
                None => 0,
            },
        )
    }

    pub fn get_request(&self, request_id: u32) -> anyhow::Result<DownloadRequest> {
        let request_id = request_id.to_be_bytes();
        let request_details = self
            .requests_by_request_id
            .get(request_id)?
            .ok_or(anyhow!("Unknown request ID"))?;
        // Depending on the context this is called from, we could also check completed requests
        Ok(DownloadRequest::from_db_key_value(
            request_id.to_vec(),
            request_details.to_vec(),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::sync::mpsc::channel;

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
        assert_eq!(dl_req.total_size, decoded_request.total_size);
        assert_eq!(dl_req.request_id, decoded_request.request_id);
        // This will fail if we compare durations due to the missing nanoseconds
        assert_eq!(
            dl_req.timestamp.as_secs(),
            decoded_request.timestamp.as_secs()
        );
        assert_eq!(dl_req.peer_public_key, decoded_request.peer_public_key);
    }

    #[tokio::test]
    async fn create_wishlist() {
        let storage = TempDir::new().unwrap();
        let mut db_dir = storage.as_ref().to_owned();
        db_dir.push("db");
        let db = sled::open(db_dir).expect("open");
        let (response_tx, _response_rx) = channel(1024);
        let wishlist = WishList::new(&db, response_tx).unwrap();

        let requested_file = RequestedFile {
            path: "books/book.pdf".to_string(),
            size: 501546,
            request_id: 5144,
        };
        let dl_req = DownloadRequest::new(
            "books/book.pdf".to_string(),
            501546,
            5144,
            *b"23lkjfsdfljkfsdlskdjsfdklfsddjsd",
        );

        wishlist.add_request(&dl_req).unwrap();

        wishlist.add_requested_file(&requested_file).unwrap();

        let requested_items = wishlist.requested().unwrap();
        for item in requested_items {
            assert_eq!(dl_req.path, item.path);
            assert_eq!(dl_req.total_size, item.total_size);
            assert_eq!(dl_req.request_id, item.request_id);
            // This will fail if we compare durations due to the missing nanoseconds
            assert_eq!(dl_req.timestamp.as_secs(), item.timestamp.as_secs());
            assert_eq!(key_to_name(&dl_req.peer_public_key), item.peer_name);
        }

        wishlist.file_completed(requested_file.clone()).unwrap();

        let downloaded_items = wishlist.downloaded().unwrap();

        for item in downloaded_items {
            assert_eq!(requested_file.path, item.path);
            assert_eq!(requested_file.size, item.size);
            assert_eq!(requested_file.request_id, item.request_id);
            // // This will fail if we compare durations due to the missing nanoseconds
            // assert_eq!(requested_file.timestamp.as_secs(), item.timestamp.as_secs());
            // assert_eq!(key_to_name(&dl_req.peer_public_key), item.peer_name);
        }
    }
}
