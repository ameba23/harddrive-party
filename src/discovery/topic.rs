use cryptoxide::{blake2b::Blake2b, digest::Digest};

// This is the Blake2b hash of the string "harddrive-party"
const CONTEXT: [u8; 32] = [
    201, 150, 87, 104, 91, 62, 47, 60, 2, 5, 31, 221, 42, 53, 91, 14, 115, 133, 124, 79, 115, 180,
    210, 81, 113, 98, 32, 171, 11, 228, 240, 2,
];

pub struct Topic {
    pub name: String,
    pub hash: [u8; 32],
}

impl Topic {
    pub fn new(name: String) -> Self {
        let mut hash = [0u8; 32];
        let mut topic_hash = Blake2b::new_keyed(32, &CONTEXT);
        topic_hash.input(name.as_str().as_bytes());
        topic_hash.result(&mut hash);

        Self { name, hash }
    }

    pub fn as_hex(&self) -> String {
        to_hex_string(self.hash)
    }
}

fn to_hex_string(bytes: [u8; 32]) -> String {
    let strs: Vec<String> = bytes.iter().take(2).map(|b| format!("{:02x}", b)).collect();
    strs.join("")
}

// fn from_hex_string(input: String) -> Vec<u8> {
//
// }
