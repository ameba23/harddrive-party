use crate::discovery::topic::Topic;
use anyhow::anyhow;
use cryptoxide::chacha20poly1305::ChaCha20Poly1305;
use rand::Rng;

type HandshakeRequest = [u8; 32 + 16 + 8];
type Token = [u8; 32];

const AAD: [u8; 0] = [];

pub fn handshake_request(topic: &Topic) -> (HandshakeRequest, Token) {
    let mut rng = rand::thread_rng();
    let token: [u8; 32] = rng.gen();
    let nonce: [u8; 8] = rng.gen();
    let key = topic.hash;
    let mut out: [u8; 32 + 16 + 8] = [0u8; 32 + 16 + 8];
    let mut tag: [u8; 16] = [0u8; 16];

    let mut cipher = ChaCha20Poly1305::new(&key, &nonce, &AAD);

    // Encrypt the msg and append the tag at the end
    cipher.encrypt(&token, &mut out[0..32], &mut tag);
    out[32..32 + 16].copy_from_slice(&tag);
    out[32 + 16..].copy_from_slice(&nonce);
    (out, token)
}

// TODO this should take a vector of possible topics
pub fn handshake_response(
    handshake_request: HandshakeRequest,
    topic: &Topic,
) -> anyhow::Result<Token> {
    let key = topic.hash;
    let mut decrypt_msg: [u8; 32] = [0u8; 32];

    let nonce = &handshake_request[32 + 16..];
    let mut cipher = ChaCha20Poly1305::new(&key, &nonce, &AAD);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_handshake() {
        let topic = Topic::new("boop".to_string());
        let (request, token) = handshake_request(&topic);
        let response = handshake_response(request, &topic).unwrap();
        assert_eq!(token, response);
    }

    #[test]
    fn handshake_fails_with_bad_topic() {
        let topic = Topic::new("boop".to_string());
        let (request, _token) = handshake_request(&topic);
        let bad_topic = Topic::new("something else".to_string());
        assert!(handshake_response(request, &bad_topic).is_err());
    }
}
