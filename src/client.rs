use crate::{ui_messages::UiServerError, wire_messages::IndexQuery};
use bincode::{deserialize, serialize};
use bytes::{Buf, Bytes, BytesMut};
use futures::{Stream, StreamExt};
use harddrive_party_shared::ui_messages::{FilesQuery, UiResponse};
use reqwest::Response;
use serde::Deserialize;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct Client {
    http_client: reqwest::Client,
    ui_address: SocketAddr,
}

impl Client {
    pub fn new(ui_address: SocketAddr) -> Self {
        Self {
            http_client: reqwest::Client::new(),
            ui_address,
        }
    }

    pub async fn shares(
        &self,
        query: IndexQuery,
    ) -> anyhow::Result<impl Stream<Item = UiResult<UiResponse>>> {
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
}

use serde::de::DeserializeOwned;

type UiResult<T> = Result<T, UiServerError>;

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
                Poll::Ready(Some(Err(_e))) => {
                    // TODO better handle error
                    return Poll::Ready(Some(Err(UiServerError::RequestError)));
                }
                Poll::Ready(None) => {
                    // End of stream
                    if this.buffer.is_empty() {
                        return Poll::Ready(None);
                    } else {
                        // Incomplete trailing data
                        // "Incomplete message at end of stream"
                        return Poll::Ready(Some(Err(UiServerError::RequestError)));
                    }
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
