//! Track requested and downloaded files
use crate::{
    hdp::{
        REQUESTED_FILES_BY_PEER, REQUESTED_FILES_BY_REQUEST_ID, REQUESTS, REQUESTS_BY_TIMESTAMP,
        REQUESTS_PROGRESS,
    },
    shares::Chunker,
    ui_messages::UiDownloadRequest,
};
use anyhow::anyhow;
use async_stream::stream;
use futures::{stream::BoxStream, StreamExt};
use harddrive_party_shared::ui_messages::UiRequestedFile;
use key_to_animal::key_to_name;
use std::time::{Duration, SystemTime};

/// How many entries to send when updating the UI
const MAX_ENTRIES_PER_MESSAGE: usize = 64;

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
        // Ignore nanoseconds
        let timestamp_secs = system_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        let timestamp = Duration::from_secs(timestamp_secs);

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

        let timestamp_secs = self.timestamp.as_secs();
        let timestamp_secs_buf = timestamp_secs.to_be_bytes();

        let request_id_buf = self.request_id.to_be_bytes();
        let total_size_buf = self.total_size.to_be_bytes();

        let mut value1 = Vec::with_capacity(8 + 8 + 32 + path_buf.len());
        value1.append(&mut timestamp_secs_buf.to_vec());
        value1.append(&mut total_size_buf.to_vec());
        value1.append(&mut self.peer_public_key.clone().to_vec());
        value1.append(&mut path_buf.to_vec());
        let kv1 = (request_id_buf.to_vec(), value1);

        let kv2 = (timestamp_secs_buf.to_vec(), request_id_buf.to_vec());
        (kv1, kv2)
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

    pub fn into_ui_download_request(self, progress: u64) -> UiDownloadRequest {
        UiDownloadRequest {
            path: self.path,
            total_size: self.total_size,
            request_id: self.request_id,
            timestamp: self.timestamp,
            peer_name: key_to_name(&self.peer_public_key),
            progress,
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
    pub downloaded: bool,
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
            downloaded: false,
        })
    }

    /// key2: `<requestid><path>` value2: `<size><downloaded>`
    fn from_db_by_request_id_key_value(key: Vec<u8>, value: Vec<u8>) -> anyhow::Result<Self> {
        let request_id_buf: [u8; 4] = key[0..4].try_into()?;
        let request_id = u32::from_be_bytes(request_id_buf);

        let path = std::str::from_utf8(&key[4..])?.to_string();

        let size_buf: [u8; 8] = value[0..8].try_into()?;
        let size = u64::from_be_bytes(size_buf);

        let downloaded = value[8] == 1;
        Ok(Self {
            path,
            size,
            request_id,
            downloaded,
        })
    }

    /// Convert to a db entry for both 'by peer' and 'by request id'
    ///
    /// key2: `<requestid><path>` value2: `<size><downloaded>`
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

        let mut key2: Vec<u8> = Vec::with_capacity(4 + path_buf.len());
        key2.append(&mut request_id_buf.to_vec());
        key2.append(&mut path_buf.to_vec());

        let mut value2: [u8; 8 + 1] = [0; 8 + 1];
        value2[0..8].copy_from_slice(&size_buf);
        let downloaded = if self.downloaded { 1 } else { 0 };
        value2[8..9].copy_from_slice(&[downloaded]);

        let kv2 = (key2, value2.to_vec());
        (kv1, kv2)
    }

    fn into_ui_requested_file(self) -> UiRequestedFile {
        UiRequestedFile {
            path: self.path,
            size: self.size,
            request_id: self.request_id,
            downloaded: self.downloaded,
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
    /// Requested files ordered by request_id
    /// We dont store bytes downloaded for individual files - only bool whether or not they are
    /// completely downloaded
    /// key: `<requestid><path>` value: `<size><downloaded>`
    requested_files_by_request_id: sled::Tree,
}

impl WishList {
    pub fn new(db: &sled::Db) -> anyhow::Result<Self> {
        Ok(WishList {
            requests_by_request_id: db.open_tree(REQUESTS)?,
            requests_by_timestamp: db.open_tree(REQUESTS_BY_TIMESTAMP)?,
            requested_files_by_peer: db.open_tree(REQUESTED_FILES_BY_PEER)?,
            bytes_downloaded_by_request_id: db.open_tree(REQUESTS_PROGRESS)?,
            requested_files_by_request_id: db.open_tree(REQUESTED_FILES_BY_REQUEST_ID)?,
        })
    }

    /// Get all requested items to send to UI
    pub fn requested(
        &self,
    ) -> anyhow::Result<Box<dyn Iterator<Item = Vec<UiDownloadRequest>> + Send + '_>> {
        let wishlist = self.clone();
        let iter = self
            .requests_by_timestamp
            .iter()
            .filter_map(move |kv_result| {
                let (_key, request_id) = kv_result.ok()?;
                let request_details = wishlist.requests_by_request_id.get(&request_id).ok()??;
                let download_request = DownloadRequest::from_db_key_value(
                    request_id.to_vec(),
                    request_details.to_vec(),
                )
                .ok()?;
                let progress = wishlist
                    .get_download_progress_for_request(u32::from_be_bytes(
                        request_id.to_vec().try_into().ok()?,
                    ))
                    .ok()?;

                Some(download_request.into_ui_download_request(progress))
            });

        let chunked = Chunker {
            inner: Box::new(iter),
            chunk_size: MAX_ENTRIES_PER_MESSAGE,
        };

        Ok(Box::new(chunked))
    }

    /// Get files associated with a given request id
    pub fn requested_files(
        &self,
        request_id: u32,
    ) -> anyhow::Result<Box<dyn Iterator<Item = Vec<UiRequestedFile>> + Send + '_>> {
        let request_id_buf = request_id.to_be_bytes();
        let iter = self
            .requested_files_by_request_id
            .scan_prefix(request_id_buf)
            .filter_map(|kv_result| match kv_result {
                Ok((key, value)) => Some(
                    RequestedFile::from_db_by_request_id_key_value(key.to_vec(), value.to_vec())
                        .ok()?
                        .into_ui_requested_file(),
                ),
                Err(_) => None,
            });

        let chunked = Chunker {
            inner: Box::new(iter),
            chunk_size: MAX_ENTRIES_PER_MESSAGE,
        };

        Ok(Box::new(chunked))
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
        Ok(())
    }

    /// Add a download request for a particular file
    /// Should be called after add_request
    pub fn add_requested_file(&self, requested_file: &RequestedFile) -> anyhow::Result<()> {
        let download_request = self.get_request(requested_file.request_id)?;

        let ((key, value), (key2, value2)) = requested_file
            .to_db_key_value(download_request.peer_public_key, download_request.timestamp);
        self.requested_files_by_peer.insert(key, value)?;
        self.requested_files_by_request_id.insert(key2, value2)?;

        Ok(())
    }

    /// Mark a particular requested file as completely downloaded
    pub fn file_completed(&self, requested_file: RequestedFile) -> anyhow::Result<bool> {
        if !requested_file.downloaded {
            return Err(anyhow!("File not marked as downloaded"));
        }
        let request_id = requested_file.request_id.to_be_bytes();
        let request_details = self
            .requests_by_request_id
            .get(request_id)?
            .ok_or(anyhow!("Unknown request ID"))?;
        let download_request =
            DownloadRequest::from_db_key_value(request_id.to_vec(), request_details.to_vec())?;
        let ((key, _value), (key2, value2)) = requested_file
            .to_db_key_value(download_request.peer_public_key, download_request.timestamp);
        self.requested_files_by_peer.remove(key)?;

        // Update download progress
        let exisitng_bytes = self.get_download_progress_for_request(requested_file.request_id)?;
        let updated_bytes = exisitng_bytes + requested_file.size;
        self.bytes_downloaded_by_request_id
            .insert(request_id, updated_bytes.to_be_bytes().to_vec())?;

        // Mark file as downloaded
        self.requested_files_by_request_id.insert(key2, value2)?;

        // Check if this was the last file associatd with this download request
        Ok(updated_bytes == download_request.total_size)
    }

    /// Given a request id, return the number of bytes downloaded so far
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

    /// Given a request id, return a request
    pub fn get_request(&self, request_id: u32) -> anyhow::Result<DownloadRequest> {
        let request_id = request_id.to_be_bytes();
        let request_details = self
            .requests_by_request_id
            .get(request_id)?
            .ok_or(anyhow!("Unknown request ID"))?;
        // Depending on the context this is called from, we could also check completed requests
        DownloadRequest::from_db_key_value(request_id.to_vec(), request_details.to_vec())
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
        assert_eq!(dl_req, decoded_request);
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
        let wishlist = WishList::new(&db).unwrap();

        let request_id = 42;

        let mut requested_file = RequestedFile {
            path: "books/book.pdf".to_string(),
            size: 501546,
            request_id,
            downloaded: false,
        };
        let dl_req = DownloadRequest::new(
            "books/book.pdf".to_string(),
            501546,
            request_id,
            *b"23lkjfsdfljkfsdlskdjsfdklfsddjsd",
        );

        wishlist.add_request(&dl_req).unwrap();

        wishlist.add_requested_file(&requested_file).unwrap();

        let mut requests = wishlist.requested().unwrap();

        assert_eq!(
            Some(vec![dl_req.into_ui_download_request(0)]),
            requests.next()
        );
        assert_eq!(None, requests.next());

        let mut requested_files = wishlist.requested_files(request_id).unwrap();

        assert_eq!(
            Some(vec![requested_file.clone().into_ui_requested_file()]),
            requested_files.next()
        );
        assert_eq!(None, requested_files.next());

        requested_file.downloaded = true;
        let request_complete = wishlist.file_completed(requested_file.clone()).unwrap();

        assert!(request_complete);
        // let mut downloaded_items = wishlist.downloaded().unwrap();
        //
        // assert_eq!(
        //     Some(requested_file.into_ui_requested_file()),
        //     downloaded_items.next()
        // );
        //
        // assert_eq!(None, downloaded_items.next());
    }
}
