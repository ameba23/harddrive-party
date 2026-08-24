use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

/// The canonical identity of a peer: its Ed25519 public key.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct PeerId([u8; Self::LENGTH]);

impl PeerId {
    pub const LENGTH: usize = 32;
    pub const ENCODED_LENGTH: usize = 43;

    pub const fn new(bytes: [u8; Self::LENGTH]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; Self::LENGTH] {
        &self.0
    }

    pub const fn into_bytes(self) -> [u8; Self::LENGTH] {
        self.0
    }

    /// A compact label for logs and terminal output. Never use this as an identifier.
    pub fn abbreviated(&self) -> String {
        self.to_string().chars().take(8).collect()
    }
}

impl From<[u8; PeerId::LENGTH]> for PeerId {
    fn from(value: [u8; PeerId::LENGTH]) -> Self {
        Self::new(value)
    }
}

impl AsRef<[u8]> for PeerId {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl FromStr for PeerId {
    type Err = PeerIdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != Self::ENCODED_LENGTH {
            return Err(PeerIdParseError::BadLength);
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|error| PeerIdParseError::Base64(error.to_string()))?;
        let bytes = decoded
            .try_into()
            .map_err(|_| PeerIdParseError::BadLength)?;
        let id = Self(bytes);
        if id.to_string() != value {
            return Err(PeerIdParseError::NonCanonical);
        }
        Ok(id)
    }
}

#[derive(Error, Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub enum PeerIdParseError {
    #[error("peer ID must be exactly 43 base64url characters")]
    BadLength,
    #[error("invalid peer ID base64url: {0}")]
    Base64(String),
    #[error("peer ID is not canonical unpadded base64url")]
    NonCanonical,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_parse_round_trip() {
        let id = PeerId::new(std::array::from_fn(|i| i as u8));
        let encoded = id.to_string();
        assert_eq!(encoded.len(), PeerId::ENCODED_LENGTH);
        assert_eq!(encoded.parse(), Ok(id));
        assert!(!encoded.contains(['+', '/', '=']));
    }

    #[test]
    fn rejects_bad_length_padding_and_alphabet() {
        assert_eq!("short".parse::<PeerId>(), Err(PeerIdParseError::BadLength));
        assert!(format!("{}=", PeerId::new([0; 32]))
            .parse::<PeerId>()
            .is_err());
        let mut invalid = PeerId::new([255; 32]).to_string();
        invalid.replace_range(..1, "+");
        assert!(invalid.parse::<PeerId>().is_err());
    }
}
