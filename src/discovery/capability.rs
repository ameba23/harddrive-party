//! Proof of knowledge of topic name
use crate::discovery::topic::Topic;
use anyhow::anyhow;
use cryptoxide::{blake2b::Blake2b, chacha20poly1305::ChaCha20Poly1305, digest::Digest};
use rand::Rng;
use std::net::SocketAddr;

const BLAKE2B_LENGTH: usize = 32;
const NONCE_LENGTH: usize = 8;
const TAG_LENGTH: usize = 16;

pub type HandshakeRequest = [u8; BLAKE2B_LENGTH + TAG_LENGTH + NONCE_LENGTH];

const AAD: [u8; 0] = [];

/// Used only for MDNS announcements
pub fn handshake_request(
    topic: &Topic,
    addr: &SocketAddr,
    public_key: &[u8; 32],
) -> HandshakeRequest {
    let key = keyed_hash(addr.to_string().as_bytes(), &topic.hash);
    let mut rng = rand::thread_rng();

    let nonce: [u8; NONCE_LENGTH] = rng.gen();
    let mut out: [u8; BLAKE2B_LENGTH + TAG_LENGTH + NONCE_LENGTH] =
        [0u8; BLAKE2B_LENGTH + TAG_LENGTH + NONCE_LENGTH];
    let mut tag: [u8; TAG_LENGTH] = [0u8; TAG_LENGTH];

    let mut cipher = ChaCha20Poly1305::new(&key, &nonce, &AAD);

    // Encrypt the msg and append the tag at the end
    cipher.encrypt(public_key, &mut out[0..BLAKE2B_LENGTH], &mut tag);
    out[BLAKE2B_LENGTH..BLAKE2B_LENGTH + TAG_LENGTH].copy_from_slice(&tag);
    out[BLAKE2B_LENGTH + TAG_LENGTH..].copy_from_slice(&nonce);
    out
}

// TODO this should take a vector of possible topics
pub fn handshake_response(
    handshake_request: HandshakeRequest,
    topic: &Topic,
    addr: SocketAddr,
) -> anyhow::Result<[u8; 32]> {
    let key = keyed_hash(addr.to_string().as_bytes(), &topic.hash);
    let mut decrypt_msg: [u8; BLAKE2B_LENGTH] = [0u8; BLAKE2B_LENGTH];

    let nonce = &handshake_request[BLAKE2B_LENGTH + TAG_LENGTH..];
    let mut cipher = ChaCha20Poly1305::new(&key, nonce, &AAD);

    if cipher.decrypt(
        &handshake_request[0..BLAKE2B_LENGTH],
        &mut decrypt_msg,
        &handshake_request[BLAKE2B_LENGTH..BLAKE2B_LENGTH + TAG_LENGTH],
    ) {
        Ok(decrypt_msg)
    } else {
        Err(anyhow!("Cannot decrypt"))
    }
}

/// Keyed blake2b hash
fn keyed_hash(input: &[u8], key: &[u8; BLAKE2B_LENGTH]) -> [u8; BLAKE2B_LENGTH] {
    let mut hash = [0u8; BLAKE2B_LENGTH];
    let mut topic_hash = Blake2b::new_keyed(BLAKE2B_LENGTH, key);
    topic_hash.input(input);
    topic_hash.result(&mut hash);
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_handshake() {
        let topic = Topic::new("boop".to_string());
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let mut rng = rand::thread_rng();
        let token: [u8; 32] = rng.gen();
        let request = handshake_request(&topic, &addr, &token);
        let response = handshake_response(request, &topic, addr).unwrap();
        assert_eq!(token, response);
    }

    #[test]
    fn handshake_fails_with_bad_topic() {
        let topic = Topic::new("boop".to_string());
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let mut rng = rand::thread_rng();
        let token: [u8; 32] = rng.gen();
        let request = handshake_request(&topic, &addr, &token);
        let bad_topic = Topic::new("something else".to_string());
        assert!(handshake_response(request, &bad_topic, addr).is_err());
    }
}
