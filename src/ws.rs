//! Websocket server / client for communication with UI
//!
//! This is optional - you can use this crate as a library and have a desktop or terminal UI which
//! communicates directly with the [crate::Hdp] instance, but the advantage of using this module is
//! that the UI can be run on a different machine than the back-end, or in the browser.

use crate::{
    hdp::SharedState,
    ui_messages::{UiEvent, UiServerError},
};
use axum::extract::ws::{Message, WebSocket};
use bincode::{deserialize, serialize};
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use harddrive_party_shared::ui_messages::PeerRemoteOrSelf;
use log::{debug, error, trace, warn};
use rand::{rngs::ThreadRng, Rng};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::{
        mpsc::{channel, Receiver, Sender},
        RwLock,
    },
};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

pub async fn handle_socket(shared_state: SharedState, mut socket: WebSocket, who: SocketAddr) {
    let mut event_receiver = shared_state.event_broadcaster.subscribe();

    // let event_cache = Arc::new(RwLock::new(Vec::<UiEvent>::new())); // TODO move into shared state
    // // Send cached messages that this client has missed out on
    // {
    //     let cache = {
    //         let cache = event_cache_clone.read().await;
    //         cache.clone()
    //     };
    //     for event in cache.iter() {
    //         let message = UiServerMessage::Event(event.clone());
    //         match serialize(&message) {
    //             Ok(message_buf) => {
    //                 if let Err(err) = outgoing.send(Message::Binary(message_buf)).await {
    //                     warn!("Cannot send ws message {:?}", err);
    //                     break;
    //                 };
    //             }
    //             Err(_) => {
    //                 error!("Cannot serialize message {message:?}");
    //             }
    //         }
    //     }
    // }
    // Give them incoming events
    //
    // Send next message from application to UI client
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

// /// Websocket server
// pub async fn server(
//     listener: TcpListener,
//     command_tx: Sender<UiClientMessage>,
//     mut response_rx: Receiver<UiServerMessage>,
// ) {
//     let state = ClientMap::new(RwLock::new(HashMap::new()));
//
//     // Loop over response channel and send to each connected client
//     let state_clone = state.clone();
//     let event_cache_clone = event_cache.clone();
//     tokio::spawn(async move {
//         while let Some(msg) = response_rx.recv().await {
//             {
//                 let mut cache = event_cache_clone.write().await;
//                 cache_event(&msg, &mut cache);
//             }
//
//             let clients = state_clone.read().await;
//             trace!("{} connected UI clients", clients.len());
//
//             for client in clients.values() {
//                 if let Err(err) = client.send(msg.clone()).await {
//                     warn!("Cannot send msg to connected client {:?}", err);
//                 };
//             }
//         }
//     });
//
//     // Accept connections from UI clients
//     while let Ok((stream, client_addr)) = listener.accept().await {
//         let (tx, mut rx) = channel(65536);
//         {
//             state.write().await.insert(client_addr, tx);
//         }
//         let state_clone = state.clone();
//         let command_tx = command_tx.clone();
//         let event_cache_clone = event_cache.clone();
//         tokio::spawn(async move {
//             let ws_stream = tokio_tungstenite::accept_async(stream)
//                 .await
//                 .expect("Error during the websocket handshake occurred");
//
//             let (mut outgoing, mut incoming) = ws_stream.split();
//
//             loop {
//                 select! {
//                     // Receive next message from UI client and send to application
//                     maybe_ws_msg = incoming.next() => {
//                         match maybe_ws_msg {
//                             Some(ws_msg) => {
//                                 if let Ok(Message::Binary(ws_msg_buf)) = ws_msg {
//
//                                     let message_result: Result<UiClientMessage, Box<bincode::ErrorKind>> =
//                                     deserialize(&ws_msg_buf);
//                                     match message_result {
//                                         Ok(message) => {
//                                             if command_tx.send(message).await.is_err() {
//                                                 warn!("WS message channel closed!");
//                                                 break;
//                                             };
//                                         }
//                                         Err(_) => {
//                                             warn!("Could not deserialize ws message");
//                                         }
//                                     };
//                                 }
//                             }
//                             None => {
//                                 break;
//                             }
//                         }
//                     }
//                     // Send next message from application to UI client
//                     Some(msg) = rx.recv() => {
//                         match serialize(&msg) {
//                             Ok(message_bytes) => {
//                                 if let Err(err) = outgoing.send(Message::Binary(message_bytes)).await {
//                                     warn!("cannot send ws message {:?}", err);
//                                     break;
//                                 };
//                             },
//                             Err(_) => {
//                                 error!("Cannot serialize message {msg:?}")
//                             }
//                         }
//                     }
//                 }
//             }
//             // Remove the client from our map
//             if state_clone.write().await.remove(&client_addr).is_none() {
//                 warn!("WS client address not removed! {}", client_addr);
//             };
//         });
//     }
// }

// /// WS client used for the CLI
// pub struct WsClient {
//     write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
//     read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
//     rng: ThreadRng,
// }
//
// impl WsClient {
//     pub async fn new(server_addr: String) -> anyhow::Result<WsClient> {
//         let (ws_stream, _) = connect_async(server_addr).await?;
//         debug!("WebSocket handshake has been successfully completed");
//
//         let (write, read) = ws_stream.split();
//         let rng = rand::thread_rng();
//
//         Ok(WsClient { write, read, rng })
//     }
//
//     pub async fn send_message(&mut self, command: Command) -> anyhow::Result<u32> {
//         let id = self.rng.gen();
//         let message = UiClientMessage { id, command };
//         let message_buf = serialize(&message)?;
//         self.write.send(Message::Binary(message_buf)).await?;
//         Ok(id)
//     }
// }
//
// /// Make a connection and send a single command to the harddrive-party instance
// /// Used by the CLI
// pub async fn single_client_command(
//     server_addr: String,
//     command: Command,
// ) -> anyhow::Result<Receiver<Result<UiResponse, UiServerError>>> {
//     let mut ws_client = WsClient::new(server_addr).await?;
//     let message_id = ws_client.send_message(command).await?;
//     Ok(read_responses(ws_client.read, message_id).await)
// }
//
// /// Read UI respnses (used internally by single_client_command)
// // TODO this should return a result with a stream of messages
// async fn read_responses(
//     mut read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
//     message_id: u32,
// ) -> Receiver<Result<UiResponse, UiServerError>> {
//     let (tx, rx) = channel(1024);
//     tokio::spawn(async move {
//         while let Some(msg_result) = read.next().await {
//             match msg_result {
//                 Ok(Message::Binary(buf)) => match deserialize(&buf) {
//                     Ok(UiServerMessage::Response { id, response }) => {
//                         if id == message_id {
//                             if tx.send(response).await.is_err() {
//                                 warn!("Ws single response channel closed");
//                                 break;
//                             };
//                         } else {
//                             warn!("Unexpected msg id - got message for another client");
//                         }
//                     }
//                     Ok(UiServerMessage::Event(event)) => match event {
//                         UiEvent::PeerConnected {
//                             name,
//                             peer_type: PeerRemoteOrSelf::Me { .. },
//                         } => {
//                             println!("{}", name);
//                         }
//                         UiEvent::PeerConnected {
//                             name,
//                             peer_type: PeerRemoteOrSelf::Remote,
//                         } => {
//                             println!("Connected to remote peer: {}", name);
//                         }
//                         _ => {
//                             println!("Got event {:?}", event);
//                         }
//                     },
//                     Err(_) => {
//                         error!("Cannot deserialize UI message");
//                     }
//                 },
//                 Err(e) => {
//                     println!("Error response {:?}", e);
//                     break;
//                 }
//                 _ => {
//                     println!("Unexpected ws message type");
//                 }
//             }
//         }
//         println!("Cannot read more responses, closing connection");
//     });
//     rx
// }
