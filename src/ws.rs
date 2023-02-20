use crate::{
    ui_messages::{Command, UiClientMessage, UiResponse, UiServerMessage},
    wire_messages::LsResponse,
};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use bincode::{deserialize, serialize};
use colored::Colorize;
use futures::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use log::warn;
use rand::{rngs::ThreadRng, Rng};
use tokio::{
    net::TcpListener,
    select,
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Tx = UnboundedSender<UiServerMessage>;
type PeerMap = Arc<Mutex<HashMap<SocketAddr, Tx>>>;

pub async fn server(
    addr: SocketAddr,
    command_tx: UnboundedSender<UiClientMessage>,
    mut response_rx: UnboundedReceiver<UiServerMessage>,
) -> anyhow::Result<()> {
    let state = PeerMap::new(Mutex::new(HashMap::new()));

    // Create the event loop and TCP listener we'll accept connections on.
    let try_socket = TcpListener::bind(&addr).await;
    let listener = try_socket.expect("Failed to bind");
    println!("WS Listening on: {}", addr);

    // Loop over response channel and send to each connected client
    let state_clone = state.clone();
    tokio::spawn(async move {
        while let Some(msg) = response_rx.recv().await {
            let clients = state_clone.lock().unwrap();
            println!("{} connected clients", clients.len());
            for client in clients.values() {
                match client.send(msg.clone()) {
                    Ok(_) => {}
                    Err(err) => {
                        println!("Cannot send msg to connected client {:?}", err);
                    }
                };
            }
        }
    });

    // Accept connections from UI clients
    while let Ok((stream, client_addr)) = listener.accept().await {
        let (tx, mut rx) = unbounded_channel();
        state.lock().unwrap().insert(client_addr, tx);

        let state_clone = state.clone();
        let command_tx = command_tx.clone();
        tokio::spawn(async move {
            println!("Incoming TCP connection from: {}", client_addr);

            let ws_stream = tokio_tungstenite::accept_async(stream)
                .await
                .expect("Error during the websocket handshake occurred");

            let (mut outgoing, mut incoming) = ws_stream.split();
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
                                println!("Ws Connection closed, ending loop");
                                break;
                            }
                        }
                    }
                    // Send next message from application to UI client
                    // TODO handle none
                    Some(msg) = rx.recv() => {
                        if let Err(err) = outgoing.send(Message::Binary(serialize(&msg).unwrap())).await {
                            println!("cannot send ws message {:?}", err);
                            break;
                        };
                    }
                }
            }
            // Remove the client from our map
            if state_clone.lock().unwrap().remove(&client_addr).is_none() {
                println!("WS client address not removed! {}", client_addr);
            };
        });
    }

    Ok(())
}

pub struct WsClient {
    write: SplitSink<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, Message>,
    read: SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>,
    rng: ThreadRng,
}

impl WsClient {
    pub async fn new(server_addr: String) -> anyhow::Result<WsClient> {
        let (ws_stream, _) = connect_async(server_addr).await.expect("Failed to connect");
        println!("WebSocket handshake has been successfully completed");

        let (write, read) = ws_stream.split();
        let rng = rand::thread_rng();

        Ok(WsClient { write, read, rng })
    }

    pub async fn send_message(&mut self, command: Command) -> anyhow::Result<u32> {
        let id = self.rng.gen();
        let message = UiClientMessage { id, command };
        let message_buf = serialize(&message).unwrap();
        self.write.send(Message::Binary(message_buf)).await?;
        Ok(id)
    }

    pub async fn read_responses(&mut self, message_id: u32) {
        while let Some(msg_result) = self.read.next().await {
            match msg_result {
                Ok(Message::Binary(buf)) => {
                    let msg: UiServerMessage = deserialize(&buf).unwrap();
                    match msg {
                        UiServerMessage::Response { id, response } => {
                            if id == message_id {
                                match response {
                                    Ok(UiResponse::Read(data)) => {
                                        println!("{:?}", data);
                                    }
                                    Ok(UiResponse::Ls(ls_response, peer_name)) => match ls_response
                                    {
                                        LsResponse::Success(entries) => {
                                            for entry in entries {
                                                if entry.is_dir {
                                                    println!(
                                                        "[{}/{}] {}",
                                                        peer_name,
                                                        entry.name.green(),
                                                        entry.size
                                                    );
                                                } else {
                                                    println!(
                                                        "{}/{} {}",
                                                        peer_name, entry.name, entry.size
                                                    );
                                                }
                                            }
                                        }
                                        LsResponse::Err(err) => {
                                            println!("Error from peer {:?}", err);
                                        }
                                    },
                                    Ok(some_other_response) => {
                                        println!("{:?}", some_other_response);
                                        break;
                                    }
                                    Err(e) => {
                                        println!("Error from server {:?}", e);
                                        break;
                                    }
                                }
                            } else {
                                println!("Unexpected msg id - got message for another client");
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
    }
}

pub async fn single_client_command(server_addr: String, command: Command) -> anyhow::Result<()> {
    let mut ws_client = WsClient::new(server_addr).await?;
    let message_id = ws_client.send_message(command).await?;
    ws_client.read_responses(message_id).await;
    // read.for_each(|message| async {
    //     let data = message.unwrap().into_data();
    //     println!("data {:?}", data);
    // });

    Ok(())
}
