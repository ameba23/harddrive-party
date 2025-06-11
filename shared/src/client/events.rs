use crate::ui_messages::UiEvent;
use bincode::deserialize;
use futures::{stream::SplitStream, Stream, StreamExt};
use reqwest::Url;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_tungstenite::{connect_async, WebSocketStream};

#[cfg(feature = "native")]
pub struct EventStream(
    SplitStream<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
);

#[cfg(feature = "native")]
impl EventStream {
    pub async fn new(mut ui_url: Url) -> anyhow::Result<Self> {
        ui_url.set_scheme("ws");
        ui_url.set_path("ws");
        let (ws_stream, _) = connect_async(ui_url).await?;
        let (_write, read) = ws_stream.split();
        Ok(Self(read))
    }
}

/// Gives a stream of events from the websocket server
#[cfg(feature = "native")]
impl Stream for EventStream {
    type Item = anyhow::Result<UiEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.0.poll_next_unpin(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(message_result)) => Poll::Ready(Some(match message_result {
                Ok(message) => match message {
                    tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                        match deserialize(&bytes) {
                            Ok(event) => Ok(event),
                            Err(_) => Err(anyhow::anyhow!("Cannot deserialize event")),
                        }
                    }
                    _ => Err(anyhow::anyhow!("Unexpected message type - expected binary")),
                },
                Err(error) => Err(error.into()),
            })),
            Poll::Ready(None) => Poll::Ready(None),
        }
    }
}
