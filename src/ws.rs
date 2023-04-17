//! Websocket server / client for communication with UI

use crate::ui_messages::{
    Command, UiClientMessage, UiEvent, UiResponse, UiServerError, UiServerMessage,
};
use bincode::{deserialize, serialize};
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use log::{debug, trace, warn};
use rand::{rngs::ThreadRng, Rng};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::{
    net::TcpListener,
    select,
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Tx = UnboundedSender<UiServerMessage>;
type ClientMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

/// WS server
pub async fn server(
    addr: SocketAddr,
    command_tx: UnboundedSender<UiClientMessage>,
    mut response_rx: UnboundedReceiver<UiServerMessage>,
) -> anyhow::Result<()> {
    let state = ClientMap::new(Mutex::new(HashMap::new()));
    let event_cache = Arc::new(Mutex::new(Vec::<UiEvent>::new()));

    // Create the event loop and TCP listener we'll accept connections on.
    let try_socket = TcpListener::bind(&addr).await;
    let listener = try_socket?;
    println!("WS Listening on: {}", addr);

    // Loop over response channel and send to each connected client
    let state_clone = state.clone();
    let event_cache_clone = event_cache.clone();
    tokio::spawn(async move {
        while let Some(msg) = response_rx.recv().await {
            let clients = state_clone.lock().unwrap();
            trace!("{} connected clients", clients.len());

            {
                let mut cache = event_cache_clone.lock().unwrap();
                cache_event(&msg, &mut cache);
            }

            for client in clients.values() {
                match client.send(msg.clone()) {
                    Ok(_) => {}
                    Err(err) => {
                        warn!("Cannot send msg to connected client {:?}", err);
                    }
                };
            }
        }
    });

    // Accept connections from UI clients
    while let Ok((stream, client_addr)) = listener.accept().await {
        let (tx, mut rx) = unbounded_channel();
        // TODO err handle
        state.lock().unwrap().insert(client_addr, tx);

        let state_clone = state.clone();
        let command_tx = command_tx.clone();
        let event_cache_clone = event_cache.clone();
        tokio::spawn(async move {
            debug!("Incoming WS connection from: {}", client_addr);

            let ws_stream = tokio_tungstenite::accept_async(stream)
                .await
                .expect("Error during the websocket handshake occurred");

            let (mut outgoing, mut incoming) = ws_stream.split();

            // Send cached messages that this client has missed out on
            {
                let cache = {
                    let cache = event_cache_clone.lock().unwrap();
                    cache.clone()
                };
                for event in cache.iter() {
                    let message = UiServerMessage::Event(event.clone());
                    let message_buf = serialize(&message).unwrap();
                    if let Err(err) = outgoing.send(Message::Binary(message_buf)).await {
                        warn!("Cannot send ws message {:?}", err);
                        break;
                    };
                }
            }

            loop {
                select! {
                    // Receive next message from UI client and send to application
                    maybe_ws_msg = incoming.next() => {
                        match maybe_ws_msg {
                            Some(ws_msg) => {
                                if let Ok(Message::Binary(ws_msg_buf)) = ws_msg {

                                    let message_result: Result<UiClientMessage, Box<bincode::ErrorKind>> =
                                    deserialize(&ws_msg_buf);
                                    match message_result {
                                        Ok(message) => {
                                            if command_tx.send(message).is_err() {
                                                warn!("WS message channel closed!");
                                                break;
                                            };
                                        }
                                        Err(_) => {
                                            warn!("Could not deserialize ws message");
                                        }
                                    };
                                }
                            }
                            None => {
                                debug!("Ws Connection closed, ending loop");
                                break;
                            }
                        }
                    }
                    // Send next message from application to UI client
                    // TODO handle none
                    Some(msg) = rx.recv() => {
                        if let Err(err) = outgoing.send(Message::Binary(serialize(&msg).unwrap())).await {
                            warn!("cannot send ws message {:?}", err);
                            break;
                        };
                    }
                }
            }
            // Remove the client from our map
            if state_clone.lock().unwrap().remove(&client_addr).is_none() {
                warn!("WS client address not removed! {}", client_addr);
            };
        });
    }

    Ok(())
}

/// WS client used for the CLI
pub struct WsClient {
    write: SplitSink<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, Message>,
    read: SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>,
    rng: ThreadRng,
}

impl WsClient {
    pub async fn new(server_addr: String) -> anyhow::Result<WsClient> {
        let (ws_stream, _) = connect_async(server_addr).await?;
        debug!("WebSocket handshake has been successfully completed");

        let (write, read) = ws_stream.split();
        let rng = rand::thread_rng();

        Ok(WsClient { write, read, rng })
    }

    pub async fn send_message(&mut self, command: Command) -> anyhow::Result<u32> {
        let id = self.rng.gen();
        let message = UiClientMessage { id, command };
        let message_buf = serialize(&message)?;
        self.write.send(Message::Binary(message_buf)).await?;
        Ok(id)
    }
}

pub async fn single_client_command(
    server_addr: String,
    command: Command,
) -> anyhow::Result<UnboundedReceiver<Result<UiResponse, UiServerError>>> {
    let mut ws_client = WsClient::new(server_addr).await?;
    let message_id = ws_client.send_message(command).await?;
    Ok(read_responses(ws_client.read, message_id).await)
    // read.for_each(|message| async {
    //     let data = message.unwrap().into_data();
    //     println!("data {:?}", data);
    // });
}

// TODO this should return a result with a stream of messages
pub async fn read_responses(
    mut read: SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>,
    message_id: u32,
) -> UnboundedReceiver<Result<UiResponse, UiServerError>> {
    let (tx, rx) = unbounded_channel();
    tokio::spawn(async move {
        while let Some(msg_result) = read.next().await {
            match msg_result {
                Ok(Message::Binary(buf)) => {
                    let msg: UiServerMessage = deserialize(&buf).unwrap();
                    match msg {
                        UiServerMessage::Response { id, response } => {
                            if id == message_id {
                                if tx.send(response).is_err() {
                                    warn!("Ws single response channel closed");
                                    break;
                                };
                            } else {
                                warn!("Unexpected msg id - got message for another client");
                            }
                        }
                        UiServerMessage::Event(event) => {
                            println!("Got event {:?}", event);
                        }
                    }
                }
                Err(e) => {
                    println!("Error response {:?}", e);
                    break;
                }
                _ => {
                    println!("Unexpected ws message type");
                }
            }
        }
        println!("Cannot read more responses, closing connection");
    });
    rx
}

// Decide which messages to cache so that clients who connect later dont miss them
fn cache_event(server_message: &UiServerMessage, cache: &mut Vec<UiEvent>) {
    if let UiServerMessage::Event(ui_event) = server_message {
        match ui_event {
            UiEvent::Uploaded(_) => {}
            UiEvent::PeerConnected { .. } => {
                cache.push(ui_event.clone());
            }
            UiEvent::PeerDisconnected { name } => {
                // Remove related PeerConnected message from cache
                cache.retain(|event| {
                    if let UiEvent::PeerConnected {
                        name: existing_name,
                        is_self: _,
                    } = event
                    {
                        name != existing_name
                    } else {
                        true
                    }
                })
            }
        }
    }
}
