//! Tracks known peers for certificate checking and reconnecting
use harddrive_party_shared::{
    codec,
    wire_messages::{AnnounceAddress, PeerConnectionDetails},
    PeerId,
};

use crate::errors::UiServerErrorWrapper;

/// Persistent store of known peers for certificate checking and reconnecting
#[derive(Debug, Clone)]
pub struct KnownPeers {
    db: sled::Tree,
}

impl KnownPeers {
    pub fn new(db: sled::Tree) -> Self {
        Self { db }
    }

    /// Add a peer who we know of through one of the discovery methods
    pub fn add_peer(&self, announce_address: &AnnounceAddress) -> Result<(), UiServerErrorWrapper> {
        let connection_details = codec::serialize(&announce_address.connection_details)?;
        self.db
            .insert(announce_address.public_key.as_bytes(), connection_details)?;
        Ok(())
    }

    /// Check if we know the exact public key during certificate verification.
    pub fn has(&self, id: &PeerId) -> bool {
        self.db.contains_key(id.as_bytes()).unwrap_or_default()
    }

    /// Iterate over known announce addresses
    pub fn iter(&self) -> Box<dyn Iterator<Item = AnnounceAddress> + Send> {
        Box::new(self.db.iter().filter_map(|kv_result| {
            let (k, v) = kv_result.ok()?;

            let public_key = PeerId::new(k.as_ref().try_into().ok()?);
            let connection_details: PeerConnectionDetails = codec::deserialize(&v).ok()?;
            Some(AnnounceAddress {
                public_key,
                connection_details,
            })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn records_are_raw_keyed_and_round_trip_connection_details() {
        let storage = TempDir::new().unwrap();
        let db = sled::open(storage.path()).unwrap();
        let tree = db.open_tree(b"k1").unwrap();
        let peers = KnownPeers::new(tree.clone());
        let address = AnnounceAddress {
            public_key: PeerId::new([17; 32]),
            connection_details: PeerConnectionDetails::NoNat("203.0.113.17:7017".parse().unwrap()),
        };

        peers.add_peer(&address).unwrap();

        assert!(peers.has(&address.public_key));
        assert!(tree.contains_key(address.public_key.as_bytes()).unwrap());
        assert_eq!(peers.iter().collect::<Vec<_>>(), vec![address]);
    }

    #[test]
    fn legacy_name_keyed_tree_is_untouched_and_ignored() {
        let storage = TempDir::new().unwrap();
        let db = sled::open(storage.path()).unwrap();
        let legacy = db.open_tree(b"k").unwrap();
        legacy.insert(b"amberCloudYak", b"legacy-record").unwrap();

        let peers = KnownPeers::new(db.open_tree(b"k1").unwrap());

        assert!(peers.iter().next().is_none());
        assert_eq!(
            legacy.get(b"amberCloudYak").unwrap().unwrap().as_ref(),
            b"legacy-record"
        );
    }
}
