//! Wire messages for communicating with other Peers
pub use crate::announce_address::{AnnounceAddress, PeerConnectionDetails};
use crate::{codec, PeerId};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use thiserror::Error;

// TODO read error

/// A request to a remote peer
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub enum Request {
    /// A request to read the remote peer's shared file index
    Ls(IndexQuery),
    /// A request to download a remote peer's file (or a portion of the file)
    Read(ReadQuery),
    /// Contact details of another peer
    AnnouncePeer(AnnouncePeer),
}

/// A request to read the remote peer's shared file index
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Eq, Hash, Default)]
pub struct IndexQuery {
    /// Base directory to query - defaults to all shared directories
    pub path: Option<String>,
    /// Filter term to search with
    pub searchterm: Option<String>,
    /// Whether to expand directories
    pub recursive: bool,
}

/// A request to download a remote peers file (or a portion of the file)
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Eq, Hash)]
pub struct ReadQuery {
    /// Path of the requested file
    pub path: String,
    /// Offset to start reading
    pub start: Option<u64>,
    /// Offset to finish reading
    pub end: Option<u64>,
}

/// A response to a `Request::Ls(IndexQuery)`
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum LsResponse {
    /// The found files or directories if the query was successful
    Success(Vec<Entry>),
    Err(LsResponseError),
}

/// A file or directory entry in a share query response
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub struct Entry {
    /// Path and filename
    pub name: String,
    /// Size in bytes
    pub size: u64,
    /// Whether this is a directory or a file
    pub is_dir: bool,
}

/// Error from making a share index query
#[derive(Error, Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum LsResponseError {
    #[error("Database error")]
    DbError,
    #[error("Path not found")]
    PathNotFound,
    #[error("Internal error: {0}")]
    InternalServer(String),
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub struct AnnouncePeer {
    pub announce_address: AnnounceAddress,
    #[serde(with = "BigArray")]
    pub signature: [u8; 64],
}

pub const ANNOUNCE_PEER_DOMAIN: &[u8] = b"harddrive-party-v1:announce-peer\0";

impl AnnouncePeer {
    pub fn signing_bytes(
        announce_address: &AnnounceAddress,
    ) -> Result<Vec<u8>, crate::codec::EncodeError> {
        let encoded = codec::serialize(announce_address)?;
        let mut message = Vec::with_capacity(ANNOUNCE_PEER_DOMAIN.len() + encoded.len());
        message.extend_from_slice(ANNOUNCE_PEER_DOMAIN);
        message.extend_from_slice(&encoded);
        Ok(message)
    }

    pub fn verify(&self) -> bool {
        let Ok(key) = VerifyingKey::from_bytes(self.announce_address.public_key.as_bytes()) else {
            return false;
        };
        let Ok(message) = Self::signing_bytes(&self.announce_address) else {
            return false;
        };
        key.verify(&message, &Signature::from_bytes(&self.signature))
            .is_ok()
    }

    pub fn peer_id(&self) -> PeerId {
        self.announce_address.public_key
    }
}

#[cfg(test)]
mod announce_tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed(address: AnnounceAddress, key: &SigningKey) -> AnnouncePeer {
        let signature = key
            .sign(&AnnouncePeer::signing_bytes(&address).unwrap())
            .to_bytes();
        AnnouncePeer {
            announce_address: address,
            signature,
        }
    }

    #[test]
    fn verifies_and_rejects_tampering_wrong_key_and_missing_domain() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let other_key = SigningKey::from_bytes(&[4; 32]);
        let address = AnnounceAddress {
            public_key: PeerId::new(key.verifying_key().to_bytes()),
            connection_details: PeerConnectionDetails::NoNat("127.0.0.1:4000".parse().unwrap()),
        };
        let announcement = signed(address.clone(), &key);
        assert!(announcement.verify());

        let mut changed_address = announcement.clone();
        changed_address.announce_address.connection_details =
            PeerConnectionDetails::NoNat("127.0.0.1:4001".parse().unwrap());
        assert!(!changed_address.verify());

        let mut changed_signature = announcement.clone();
        changed_signature.signature[0] ^= 1;
        assert!(!changed_signature.verify());

        let wrong_key_address = AnnounceAddress {
            public_key: PeerId::new(other_key.verifying_key().to_bytes()),
            ..address.clone()
        };
        assert!(!AnnouncePeer {
            announce_address: wrong_key_address,
            signature: announcement.signature,
        }
        .verify());

        let no_domain_signature = key.sign(&codec::serialize(&address).unwrap()).to_bytes();
        assert!(!AnnouncePeer {
            announce_address: address,
            signature: no_domain_signature,
        }
        .verify());
    }

    #[test]
    fn truncated_signature_cannot_deserialize() {
        let key = SigningKey::from_bytes(&[8; 32]);
        let address = AnnounceAddress {
            public_key: PeerId::new(key.verifying_key().to_bytes()),
            connection_details: PeerConnectionDetails::Symmetric("127.0.0.1".parse().unwrap()),
        };
        let mut encoded = codec::serialize(&signed(address, &key)).unwrap();
        encoded.pop();
        assert!(codec::deserialize::<AnnouncePeer>(&encoded).is_err());
    }
}
