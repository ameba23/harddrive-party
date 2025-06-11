#[cfg(feature = "native")]
mod events;

#[cfg(feature = "native")]
pub use events::EventStream;

use crate::{
    ui_messages::{FilesQuery, Info, UiDownloadRequest, UiRequestedFile, UiServerError},
    wire_messages::{IndexQuery, LsResponse, ReadQuery},
};
use bincode::{deserialize, serialize};
use bytes::{Buf, Bytes, BytesMut};
use futures::Stream;
use reqwest::{Response, Url};
use serde::de::DeserializeOwned;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A result for which the error is UiServerError
type UiResult<T> = Result<T, UiServerError>;

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
    pub async fn event_stream(&self) -> anyhow::Result<EventStream> {
        EventStream::new(self.ui_url.clone()).await
    }

    pub async fn shares(
        &self,
        query: IndexQuery,
    ) -> anyhow::Result<impl Stream<Item = Result<LsResponse, UiServerError>>> {
        let res = self
            .http_client
            .post(self.ui_url.join("shares")?)
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
    ) -> anyhow::Result<impl Stream<Item = UiResult<(LsResponse, String)>>> {
        let res = self
            .http_client
            .post(self.ui_url.join("files")?)
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
            .post(self.ui_url.join("download")?)
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
            .post(self.ui_url.join("connect")?)
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
            .post(self.ui_url.join("read")?)
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
            .get(self.ui_url.join("info")?)
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
            .get(self.ui_url.join("request")?)
            .query(&[("id", id.to_string())])
            .send()
            .await?;

        if !res.status().is_success() {
            eprintln!("Request failed with status: {}", res.status());
            return Err(anyhow::anyhow!("Request failed"));
        }

        Ok(LengthPrefixedStream::new(res))
    }

    pub async fn requests(
        &self,
    ) -> anyhow::Result<impl Stream<Item = Result<Vec<UiDownloadRequest>, UiServerError>>> {
        let res = self
            .http_client
            .get(self.ui_url.join("requests")?)
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
