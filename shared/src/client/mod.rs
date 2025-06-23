#[cfg(feature = "native")]
mod events;

#[cfg(feature = "native")]
pub use events::EventStream;

use crate::{
    ui_messages::{FilesQuery, Info, PeerPath, UiDownloadRequest, UiRequestedFile, UiServerError},
    wire_messages::{IndexQuery, LsResponse, ReadQuery},
};
use bincode::{deserialize, serialize};
use bytes::{Buf, Bytes, BytesMut};
use futures::Stream;
use reqwest::{Response, Url};
use serde::de::DeserializeOwned;
use std::task::{Context, Poll};
use std::{num::ParseIntError, pin::Pin};
use thiserror::Error;

/// A result for which the error is UiServerError
type UiResult<T> = Result<T, UiServerError>;

#[derive(Clone)]
pub struct Client {
    http_client: reqwest::Client,
    ui_url: Url,
}

// TODO error handle for all these methods
impl Client {
    pub fn new(ui_url: Url) -> Self {
        Self {
            http_client: reqwest::Client::new(),
            ui_url,
        }
    }

    #[cfg(feature = "native")]
    pub async fn event_stream(&self) -> Result<EventStream, ClientError> {
        EventStream::new(self.ui_url.clone()).await
    }

    pub async fn shares(
        &self,
        query: IndexQuery,
    ) -> Result<impl Stream<Item = Result<LsResponse, UiServerError>>, ClientError> {
        let res = self
            .http_client
            .post(
                self.ui_url
                    .join("shares")
                    .map_err(|_| ClientError::InvalidUrl)?,
            )
            .body(serialize(&query)?)
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(ClientError::HttpRequest(format!(
                "Request failed: {} {}",
                res.status(),
                res.text().await.unwrap_or_default()
            )));
        }

        Ok(LengthPrefixedStream::new(res))
    }

    pub async fn files(
        &self,
        query: FilesQuery,
    ) -> Result<impl Stream<Item = UiResult<(LsResponse, String)>>, ClientError> {
        let res = self
            .http_client
            .post(
                self.ui_url
                    .join("files")
                    .map_err(|_| ClientError::InvalidUrl)?,
            )
            .body(serialize(&query)?)
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(ClientError::HttpRequest(format!(
                "Request failed: {} {}",
                res.status(),
                res.text().await.unwrap_or_default()
            )));
        }

        Ok(LengthPrefixedStream::new(res))
    }

    pub async fn download(&self, peer_path: &PeerPath) -> Result<u32, ClientError> {
        let res = self
            .http_client
            .post(
                self.ui_url
                    .join("download")
                    .map_err(|_| ClientError::InvalidUrl)?,
            )
            .body(serialize(peer_path)?)
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(ClientError::HttpRequest(format!(
                "Request failed: {} {}",
                res.status(),
                res.text().await.unwrap_or_default()
            )));
        }

        Ok(res.text().await?.parse()?)
    }

    pub async fn connect(&self, announce_address: String) -> Result<(), ClientError> {
        let res = self
            .http_client
            .post(
                self.ui_url
                    .join("connect")
                    .map_err(|_| ClientError::InvalidUrl)?,
            )
            .body(announce_address)
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(ClientError::HttpRequest(format!(
                "Request failed: {} {}",
                res.status(),
                res.text().await.unwrap_or_default()
            )));
        }
        Ok(())
    }

    pub async fn read(
        &self,
        peer_name: String,
        read_query: ReadQuery,
    ) -> Result<impl Stream<Item = Result<Bytes, reqwest::Error>>, ClientError> {
        // payload is (read_query, peer_name)
        let res = self
            .http_client
            .post(
                self.ui_url
                    .join("read")
                    .map_err(|_| ClientError::InvalidUrl)?,
            )
            .body(serialize(&(read_query, peer_name))?)
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(ClientError::HttpRequest(format!(
                "Request failed: {} {}",
                res.status(),
                res.text().await.unwrap_or_default()
            )));
        }

        let stream = res.bytes_stream();
        Ok(stream)
    }

    pub async fn info(&self) -> Result<Info, ClientError> {
        let res = self
            .http_client
            .get(
                self.ui_url
                    .join("info")
                    .map_err(|_| ClientError::InvalidUrl)?,
            )
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(ClientError::HttpRequest(format!(
                "Request failed: {} {}",
                res.status(),
                res.text().await.unwrap_or_default()
            )));
        }

        Ok(bincode::deserialize(&res.bytes().await?)?)
    }

    pub async fn requested_files(
        &self,
        id: u32,
    ) -> Result<impl Stream<Item = Result<Vec<UiRequestedFile>, UiServerError>>, ClientError> {
        let res = self
            .http_client
            .get(
                self.ui_url
                    .join("request")
                    .map_err(|_| ClientError::InvalidUrl)?,
            )
            .query(&[("id", id.to_string())])
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(ClientError::HttpRequest(format!(
                "Request failed: {} {}",
                res.status(),
                res.text().await.unwrap_or_default()
            )));
        }

        Ok(LengthPrefixedStream::new(res))
    }

    pub async fn requests(
        &self,
    ) -> Result<impl Stream<Item = Result<Vec<UiDownloadRequest>, UiServerError>>, ClientError>
    {
        let res = self
            .http_client
            .get(
                self.ui_url
                    .join("requests")
                    .map_err(|_| ClientError::InvalidUrl)?,
            )
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(ClientError::HttpRequest(format!(
                "Request failed: {} {}",
                res.status(),
                res.text().await.unwrap_or_default()
            )));
        }

        Ok(LengthPrefixedStream::new(res))
    }

    pub async fn add_share(&self, share_dir: String) -> Result<u32, ClientError> {
        let res = self
            .http_client
            .put(
                self.ui_url
                    .join("shares")
                    .map_err(|_| ClientError::InvalidUrl)?,
            )
            .body(share_dir)
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(ClientError::HttpRequest(format!(
                "Request failed: {} {}",
                res.status(),
                res.text().await.unwrap_or_default()
            )));
        }

        Ok(res.text().await?.parse()?)
    }
    // TODO delete /shares
}

/// For deserializing chunked byte HTTP responses
// If using this with tokio spawned tasks, we also need `+ Send` - but adding send wont compile on
// wasm, so we need conditional comilation
struct LengthPrefixedStream<T>
where
    T: DeserializeOwned + 'static + Send,
{
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>>>>,
    buffer: BytesMut,
    _marker: std::marker::PhantomData<T>,
}

impl<T> LengthPrefixedStream<T>
where
    T: DeserializeOwned + 'static + Send,
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
    T: DeserializeOwned + 'static + std::marker::Unpin + Send,
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
                    let res: UiResult<T> = match deserialize(&msg) {
                        Ok(res) => res,
                        Err(err) => Err(UiServerError::Serialization(err.to_string())),
                    };
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

/// An error from the client
#[derive(PartialEq, Debug, Error)]
pub enum ClientError {
    #[error("Cannot connect: {0}")]
    ConnectionError(String),
    #[error("Invalid Url")]
    InvalidUrl,
    #[error("Unexpected message type on websocket")]
    UnexpectedMessageType,
    #[error("Serialization or deserialization: {0}")]
    Serialization(String),
    #[error("HTTP client: {0}")]
    HttpRequest(String),
    #[error("Cannot parse integer: {0}")]
    ParseInt(#[from] ParseIntError),
}

impl From<bincode::Error> for ClientError {
    fn from(value: bincode::Error) -> Self {
        ClientError::Serialization(value.to_string())
    }
}

impl From<reqwest::Error> for ClientError {
    fn from(value: reqwest::Error) -> Self {
        ClientError::Serialization(value.to_string())
    }
}
