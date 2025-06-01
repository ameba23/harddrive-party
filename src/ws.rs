//! Websocket server for sending events to UI
use crate::hdp::SharedState;
use axum::extract::ws::{Message, WebSocket};
use bincode::serialize;
use log::{error, warn};
use std::net::SocketAddr;

pub async fn handle_socket(shared_state: SharedState, mut socket: WebSocket, _who: SocketAddr) {
    let mut event_receiver = shared_state.event_broadcaster.subscribe();

    while let Ok(msg) = event_receiver.recv().await {
        match serialize(&msg) {
            Ok(message_bytes) => {
                if let Err(err) = socket.send(Message::Binary(message_bytes.into())).await {
                    warn!("cannot send ws message {:?}", err);
                    break;
                };
            }
            Err(_) => {
                error!("Cannot serialize message {msg:?}")
            }
        }
    }
}
