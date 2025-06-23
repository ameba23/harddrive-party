use crate::{client::ClientError, ui_messages::UiEvent};
use bincode::deserialize;
use futures::{stream::SplitStream, Stream, StreamExt};
use reqwest::Url;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_tungstenite::{connect_async, WebSocketStream};

#[cfg(feature = "native")]
pub struct EventStream(
    SplitStream<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
);

#[cfg(feature = "native")]
impl EventStream {
    pub async fn new(mut ui_url: Url) -> Result<Self, ClientError> {
        ui_url
            .set_scheme("ws")
            .map_err(|_| ClientError::InvalidUrl)?;
        ui_url.set_path("ws");
        let (ws_stream, _) = connect_async(ui_url)
            .await
            .map_err(|err| ClientError::ConnectionError(err.to_string()))?;
        let (_write, read) = ws_stream.split();
        Ok(Self(read))
    }
}

/// Gives a stream of events from the websocket server
#[cfg(feature = "native")]
impl Stream for EventStream {
    type Item = Result<UiEvent, ClientError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.0.poll_next_unpin(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(message_result)) => Poll::Ready(Some(match message_result {
                Ok(message) => match message {
                    tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                        Ok(deserialize(&bytes)?)
                    }
                    _ => Err(ClientError::UnexpectedMessageType),
                },
                Err(error) => Err(ClientError::ConnectionError(error.to_string())),
            })),
            Poll::Ready(None) => Poll::Ready(None),
        }
    }
}
