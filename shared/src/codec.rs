//! Wrappers exposing bincode-next codec with bincode 1 legacy format
use serde::{de::DeserializeOwned, Serialize};

pub use bincode_next::error::{DecodeError, EncodeError};

/// Wraps bincode-next encoder
pub fn serialize<T>(value: &T) -> Result<Vec<u8>, EncodeError>
where
    T: Serialize,
{
    bincode_next::serde::encode_to_vec(value, bincode_next::config::legacy())
}

/// Wraps bincode-next decoder
pub fn deserialize<T>(bytes: impl AsRef<[u8]>) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
{
    let (value, bytes_read) =
        bincode_next::serde::decode_from_slice(bytes.as_ref(), bincode_next::config::legacy())?;
    if bytes_read != bytes.as_ref().len() {
        return Err(DecodeError::OtherString(format!(
            "trailing bytes after bincode message: {}",
            bytes.as_ref().len() - bytes_read
        )));
    }
    Ok(value)
}
