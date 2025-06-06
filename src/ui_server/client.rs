use crate::{ui_messages::UiServerError, wire_messages::IndexQuery};
use bincode::{deserialize, serialize};
use bytes::{Buf, Bytes, BytesMut};
use futures::stream::SplitStream;
use futures::{Stream, StreamExt};
use harddrive_party_shared::ui_messages::{FilesQuery, Info, UiEvent, UiRequestedFile, UiResponse};
use harddrive_party_shared::wire_messages::{LsResponse, ReadQuery};
use reqwest::Response;
use serde::de::DeserializeOwned;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_tungstenite::{connect_async, WebSocketStream};

/// A result for which the error is UiServerError
type UiResult<T> = Result<T, UiServerError>;

pub struct Client {
    http_client: reqwest::Client,
    ui_address: SocketAddr,
    events: SplitStream<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
}

// TODO error handle for all these methods
impl Client {
    pub async fn new(ui_address: SocketAddr) -> anyhow::Result<Self> {
        let (ws_stream, _) = connect_async(format!("ws://{ui_address}/ws")).await?;
        let (_write, read) = ws_stream.split();
        Ok(Self {
            http_client: reqwest::Client::new(),
            ui_address,
            events: read,
        })
    }

    pub async fn shares(
        &self,
        query: IndexQuery,
    ) -> anyhow::Result<impl Stream<Item = Result<LsResponse, UiServerError>>> {
        let res = self
            .http_client
            .post(format!("http://{}/shares", self.ui_address))
            .body(serialize(&query)?)
            .send()
            .await?;

        if !res.status().is_success() {
            eprintln!("Request failed with status: {}", res.status());
            return Err(anyhow::anyhow!("Request failed"));
        }

        Ok(LengthPrefixedStream::new(res))
    }

    pub async fn files(
        &self,
        query: FilesQuery,
    ) -> anyhow::Result<impl Stream<Item = UiResult<UiResponse>>> {
        let res = self
            .http_client
            .post(format!("http://{}/files", self.ui_address))
            .body(serialize(&query)?)
            .send()
            .await?;

        if !res.status().is_success() {
            eprintln!("Request failed with status: {}", res.status());
            return Err(anyhow::anyhow!("Request failed"));
        }

        Ok(LengthPrefixedStream::new(res))
    }

    pub async fn download(&self, path: String, peer_name: String) -> anyhow::Result<u32> {
        let res = self
            .http_client
            .post(format!("http://{}/download", self.ui_address))
            .body(serialize(&(path, peer_name))?)
            .send()
            .await?;

        if !res.status().is_success() {
            eprintln!("Request failed with status: {}", res.status());
            return Err(anyhow::anyhow!("Request failed"));
        }
        Ok(res.text().await?.parse()?)
    }

    pub async fn connect(&self, announce_address: String) -> anyhow::Result<()> {
        let res = self
            .http_client
            .post(format!("http://{}/connect", self.ui_address))
            .body(announce_address)
            .send()
            .await?;

        if !res.status().is_success() {
            eprintln!("Request failed with status: {}", res.status());
            return Err(anyhow::anyhow!("Request failed"));
        }
        Ok(())
    }

    pub async fn read(
        &self,
        peer_name: String,
        read_query: ReadQuery,
    ) -> anyhow::Result<impl Stream<Item = Result<Bytes, reqwest::Error>>> {
        // payload is (read_query, peer_name)
        let res = self
            .http_client
            .post(format!("http://{}/read", self.ui_address))
            .body(serialize(&(read_query, peer_name))?)
            .send()
            .await?;

        if !res.status().is_success() {
            eprintln!("Request failed with status: {}", res.status());
            return Err(anyhow::anyhow!("Request failed"));
        }

        let stream = res.bytes_stream();
        Ok(stream)
    }

    pub async fn info(&self) -> anyhow::Result<Info> {
        let res = self
            .http_client
            .get(format!("http://{}/info", self.ui_address))
            .send()
            .await?;

        Ok(bincode::deserialize(&res.bytes().await?)?)
    }

    pub async fn requested_files(
        &self,
        id: u32,
    ) -> anyhow::Result<impl Stream<Item = Result<Vec<UiRequestedFile>, UiServerError>>> {
        let res = self
            .http_client
            .get(format!("http://{}/request", self.ui_address))
            .query(&[("id", id.to_string())])
            .send()
            .await?;

        if !res.status().is_success() {
            eprintln!("Request failed with status: {}", res.status());
            return Err(anyhow::anyhow!("Request failed"));
        }

        Ok(LengthPrefixedStream::new(res))
    }

    // TODO put /shares
    // TODO delete /shares
    // TODO get /requests
    // TODO get /request
}

/// For deserializing chunked byte HTTP responses
struct LengthPrefixedStream<T>
where
    T: DeserializeOwned + Send + 'static,
{
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buffer: BytesMut,
    _marker: std::marker::PhantomData<T>,
}

impl<T> LengthPrefixedStream<T>
where
    T: DeserializeOwned + Send + 'static,
{
    pub fn new(response: Response) -> Self {
        let stream = response.bytes_stream();
        LengthPrefixedStream {
            inner: Box::pin(stream),
            buffer: BytesMut::with_capacity(64 * 1024),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> Stream for LengthPrefixedStream<T>
where
    T: DeserializeOwned + Send + 'static + std::marker::Unpin,
{
    type Item = UiResult<T>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            // Try to parse a complete message
            if this.buffer.len() >= 4 {
                let mut len_buf = &this.buffer[..4];
                let msg_len = len_buf.get_u32() as usize;

                if this.buffer.len() >= 4 + msg_len {
                    this.buffer.advance(4); // discard the length prefix
                    let msg = this.buffer.split_to(msg_len);
                    let res: UiResult<T> = deserialize(&msg).unwrap();
                    return Poll::Ready(Some(res));
                }
            }

            // Not enough data - try to pull the next chunk
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.extend_from_slice(&chunk);
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(UiServerError::RequestError(e.to_string()))));
                }
                Poll::Ready(None) => {
                    // End of stream
                    if this.buffer.is_empty() {
                        return Poll::Ready(None);
                    } else {
                        // Incomplete trailing data
                        return Poll::Ready(Some(Err(UiServerError::RequestError(
                            "Incomlete message at end of stream".to_string(),
                        ))));
                    }
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Gives a stream of events from the websocket server
impl Stream for Client {
    type Item = anyhow::Result<UiEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.events.poll_next_unpin(cx) {
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
