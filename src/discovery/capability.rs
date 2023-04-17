use crate::discovery::topic::Topic;
use anyhow::anyhow;
use cryptoxide::{blake2b::Blake2b, chacha20poly1305::ChaCha20Poly1305, digest::Digest};
use rand::Rng;
use std::net::SocketAddr;

use super::SessionToken;

pub type HandshakeRequest = [u8; 32 + 16 + 8];

const AAD: [u8; 0] = [];

/// Used only for MDNS announcements
pub fn handshake_request(
    topic: &Topic,
    addr: &SocketAddr,
    token: &SessionToken,
) -> HandshakeRequest {
    let key = keyed_hash(addr.to_string().as_str().as_bytes(), &topic.hash);
    let mut rng = rand::thread_rng();

    let nonce: [u8; 8] = rng.gen();
    let mut out: [u8; 32 + 16 + 8] = [0u8; 32 + 16 + 8];
    let mut tag: [u8; 16] = [0u8; 16];

    let mut cipher = ChaCha20Poly1305::new(&key, &nonce, &AAD);

    // Encrypt the msg and append the tag at the end
    cipher.encrypt(token, &mut out[0..32], &mut tag);
    out[32..32 + 16].copy_from_slice(&tag);
    out[32 + 16..].copy_from_slice(&nonce);
    out
}

// TODO this should take a vector of possible topics
pub fn handshake_response(
    handshake_request: HandshakeRequest,
    topic: &Topic,
    addr: SocketAddr,
) -> anyhow::Result<SessionToken> {
    let key = keyed_hash(addr.to_string().as_str().as_bytes(), &topic.hash);
    let mut decrypt_msg: [u8; 32] = [0u8; 32];

    let nonce = &handshake_request[32 + 16..];
    let mut cipher = ChaCha20Poly1305::new(&key, nonce, &AAD);

    if cipher.decrypt(
        &handshake_request[0..32],
        &mut decrypt_msg,
        &handshake_request[32..32 + 16],
    ) {
        Ok(decrypt_msg)
    } else {
        Err(anyhow!("Cannot decrypt"))
    }
}

fn keyed_hash(input: &[u8], key: &[u8; 32]) -> [u8; 32] {
    let mut hash = [0u8; 32];
    let mut topic_hash = Blake2b::new_keyed(32, key);
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
