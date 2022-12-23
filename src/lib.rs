use prost::Message;
use std::result::Result as ResultResult;

pub mod messages {
    include!(concat!(env!("OUT_DIR"), "/harddriveparty.messages.rs"));
}

pub mod fs;
pub mod protocol;
pub mod rpc;
pub mod run;
pub mod shares;

pub fn serialize_message(message: &messages::HdpMessage) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.reserve(message.encoded_len());
    // Unwrap is safe, since we have reserved sufficient capacity in the vector.
    message.encode(&mut buf).unwrap();
    buf
}

pub fn deserialize_message(buf: &[u8]) -> ResultResult<messages::HdpMessage, prost::DecodeError> {
    // messages::HdpMessage::decode(&mut Cursor::new(buf))
    messages::HdpMessage::decode(buf)
}
