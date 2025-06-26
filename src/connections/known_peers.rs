//! Tracks known peers for certificate checking and reconnecting
use harddrive_party_shared::wire_messages::AnnounceAddress;

use crate::errors::UiServerErrorWrapper;

/// Persistent store of known peers for certificate checking and reconnecting
#[derive(Clone)]
pub struct KnownPeers {
    db: sled::Tree,
}

impl KnownPeers {
    pub fn new(db: sled::Tree) -> Self {
        Self { db }
    }

    /// Add a peer who we know of through one of the discovery methods
    pub fn add_peer(&self, announce_address: &AnnounceAddress) -> Result<(), UiServerErrorWrapper> {
        let connection_details = bincode::serialize(&announce_address.connection_details)?;
        self.db
            .insert(announce_address.name.as_bytes(), connection_details)?;
        Ok(())
    }

    /// Check if we know of a given peer name during ceritication verification
    pub fn has(&self, name: &str) -> bool {
        self.db.contains_key(name.as_bytes()).unwrap_or_default()
    }

    /// Iterate over known announce addresses
    pub fn iter(&self) -> Box<dyn Iterator<Item = AnnounceAddress> + Send> {
        Box::new(self.db.iter().filter_map(|kv_result| {
            let (k, v) = kv_result.ok()?;

            Some(AnnounceAddress {
                name: String::from_utf8(k.to_vec()).ok()?,
                connection_details: bincode::deserialize(&v).ok()?,
            })
        }))
    }
}
