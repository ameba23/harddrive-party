use crate::AppError;
use anyhow::anyhow;
use bincode::deserialize;
use futures::{
    channel::mpsc::{channel, Receiver},
    SinkExt, StreamExt,
};
use harddrive_party_shared::ui_messages::UiEvent;
use leptos::prelude::*;
use log::{debug, error, warn};
use reqwasm::websocket::{futures::WebSocket, Message};
use std::collections::HashSet;
use wasm_bindgen_futures::spawn_local;

#[derive(Clone, Debug)]
pub struct WebsocketService;

impl WebsocketService {
    pub fn new(
        url: url::Url,
        set_error_message: WriteSignal<HashSet<AppError>>,
    ) -> anyhow::Result<(Self, Receiver<UiEvent>)> {
        let mut url = url.join("ws")?;
        url.set_scheme("ws")
            .map_err(|_| anyhow!("Cannot set url scheme"))?;

        let (out_tx, out_rx) = channel::<UiEvent>(1024);
        spawn_local(Self::run_ws_loop(url, set_error_message, out_tx));

        Ok((Self, out_rx))
    }

    async fn run_ws_loop(
        url: url::Url,
        set_error_message: WriteSignal<HashSet<AppError>>,
        mut out_tx: futures::channel::mpsc::Sender<UiEvent>,
    ) {
        let mut retry_delay = std::time::Duration::from_secs(3);

        loop {
            debug!("Connecting to ws {}", url);
            match WebSocket::open(&url.to_string()) {
                Ok(ws) => {
                    set_error_message.update(|errors| {
                        errors.remove(&AppError::WsConnection);
                    });
                    let (_write, mut read) = ws.split();

                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(Message::Bytes(buf)) => match deserialize::<UiEvent>(&buf) {
                                Ok(event) => {
                                    debug!("Decoded msg from server {:?}", event);
                                    if out_tx.send(event).await.is_err() {
                                        error!("Cannot send ws message over channel");
                                        break;
                                    }
                                }
                                Err(e) => {
                                    error!("Deserialization failed: {:?}", e);
                                }
                            },
                            Ok(Message::Text(text)) => {
                                warn!("Unexpected text from WebSocket: {}", text);
                            }
                            Err(e) => {
                                error!("WebSocket error: {:?}", e);
                                break;
                            }
                        }
                    }

                    debug!(
                        "WebSocket connection closed. Reconnecting in {} seconds...",
                        retry_delay.as_secs()
                    );
                }
                Err(e) => {
                    error!("Failed to connect: {:?}", e);
                }
            }

            // Update error state and sleep before retry
            set_error_message.update(|errors| {
                errors.insert(AppError::WsConnection);
            });

            gloo_timers::future::sleep(retry_delay).await;
            retry_delay = std::cmp::min(retry_delay * 2, std::time::Duration::from_secs(30));
        }
    }
}
