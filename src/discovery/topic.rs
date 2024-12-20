//! Topic name for connecting peers

use cryptoxide::{blake2b::Blake2b, chacha20poly1305::ChaCha20Poly1305, digest::Digest};
use rand::{thread_rng, Rng};

const AAD: [u8; 0] = [];

/// This is the Blake2b hash of the string "harddrive-party"
/// it is hashed together with the topic name
const CONTEXT: [u8; 32] = [
    201, 150, 87, 104, 91, 62, 47, 60, 2, 5, 31, 221, 42, 53, 91, 14, 115, 133, 124, 79, 115, 180,
    210, 81, 113, 98, 32, 171, 11, 228, 240, 2,
];

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct Topic {
    pub name: String,
    pub hash: [u8; 32],
    /// A publically sharable ID made by hashing a second time
    /// and encoding as hex
    pub public_id: String,
}

impl Topic {
    pub fn new(name: String) -> Self {
        let mut hash = [0u8; 32];
        let mut topic_hash = Blake2b::new_keyed(32, &CONTEXT);
        topic_hash.input(name.as_bytes());
        topic_hash.result(&mut hash);

        let mut id_hash = [0u8; 32];
        let mut blake = Blake2b::new(32);
        blake.input(&hash);
        blake.result(&mut id_hash);
        let public_id = hex::encode(id_hash);

        Self {
            name,
            hash,
            public_id,
        }
    }

    /// Get the topic hash as hex
    pub fn as_hex(&self) -> String {
        hex::encode(self.hash)
    }

    /// Encrypt a message using this topic as the key
    pub fn encrypt(&self, payload: &[u8]) -> Vec<u8> {
        let nonce: [u8; 8] = thread_rng().gen();
        let mut out: Vec<u8> = Vec::with_capacity(payload.len() + 16 + 8);
        out.resize(payload.len(), 0);
        let mut tag: [u8; 16] = [0u8; 16];

        let mut cipher = ChaCha20Poly1305::new(&self.hash, &nonce, &AAD);

        // Encrypt the msg and append the tag at the end
        cipher.encrypt(payload, &mut out, &mut tag);
        out.append(&mut tag.to_vec());
        out.append(&mut nonce.to_vec());
        out
    }

    /// Decrypt a message using this topic as the key
    pub fn decrypt(&self, encrypted: &[u8]) -> Option<Vec<u8>> {
        if encrypted.len() < 16 + 8 {
            return None;
        };
        let ciphertext_length = encrypted.len() - 16 - 8;
        let mut plain: Vec<u8> = vec![0; ciphertext_length];

        let nonce = &encrypted[encrypted.len() - 8..];
        let mut cipher = ChaCha20Poly1305::new(&self.hash, nonce, &AAD);

        if cipher.decrypt(
            &encrypted[0..ciphertext_length],
            &mut plain,
            &encrypted[ciphertext_length..ciphertext_length + 16],
        ) {
            Some(plain)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_to_topic() {
        let topic = Topic::new("robot".to_string());

        let plain = b"beep boop".to_vec();
        let ciphertext = topic.encrypt(&plain);
        let decrypted = topic.decrypt(&ciphertext).unwrap();
        assert_eq!(plain, decrypted);
    }
}
