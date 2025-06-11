use crate::AppError;
use bincode::{deserialize, serialize};
use futures::{
    channel::mpsc::{channel, Receiver, Sender},
    SinkExt, StreamExt,
};
use harddrive_party_shared::ui_messages::UiEvent;
use leptos::prelude::*;
use log::{debug, error, warn};
use rand::{rngs::StdRng, Rng, SeedableRng};
use reqwasm::websocket::{futures::WebSocket, Message};
use std::collections::{HashMap, HashSet};
use wasm_bindgen_futures::spawn_local;

#[derive(Clone, Debug)]
pub struct WebsocketService;

impl WebsocketService {
    pub fn new(
        url: &str,
        set_error_message: WriteSignal<HashSet<AppError>>,
    ) -> anyhow::Result<(Self, Receiver<UiEvent>)> {
        let ws = WebSocket::open(url)?;

        let (mut write, mut read) = ws.split();

        let (mut out_tx, out_rx) = channel::<UiEvent>(1024);

        // Incoming messages from the server
        spawn_local(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Bytes(buf)) => {
                        let msg: UiEvent = deserialize(&buf).unwrap();
                        debug!("Decoded msg from server {:?}", msg);
                        if out_tx.send(msg).await.is_err() {
                            error!("Cannot send ws message over channel");
                            break;
                        }
                    }
                    Ok(Message::Text(text)) => {
                        warn!("Got unexpected text from websocket: {}", text);
                    }
                    Err(e) => {
                        error!("ws: {:?}", e);
                        set_error_message.update(|error_messages| {
                            error_messages.insert(AppError::WsConnection);
                        });
                    }
                }
            }
            debug!("WebSocket Closed");
        });

        Ok((Self, out_rx))
    }
}

// /// Keeps track of requests to the server over ws
// #[derive(Clone, Debug)]
// pub struct Requester {
//     ws_service: WebsocketService,
//     requests: HashMap<u32, Command>,
//     rng: StdRng,
// }
//
// impl Requester {
//     pub fn new(ws_service: WebsocketService) -> Self {
//         Self {
//             ws_service,
//             requests: HashMap::new(),
//             rng: StdRng::from_entropy(),
//         }
//     }
//
//     /// Retrieve a request by id
//     pub fn get_request(&self, id: &u32) -> Option<&Command> {
//         self.requests.get(id)
//     }
//
//     /// Create a request, keep a record of it, and send it to the server over ws
//     pub fn make_request(&mut self, command: Command) {
//         let id = self.rng.gen();
//         let command_clone = command.clone();
//         self.requests.insert(id, command_clone);
//         if self
//             .ws_service
//             .tx
//             .try_send(UiClientMessage { id, command })
//             .is_err()
//         {
//             error!("Cannot send command over channel");
//         }
//     }
//
//     /// Remove a request that has been responded to
//     pub fn remove_request(&mut self, id: &u32) {
//         self.requests.remove(id);
//     }
// }
