use crate::messages;
use crate::messages::{request, response};
use async_std::fs;
use futures_lite::AsyncReadExt;
use futures_lite::AsyncSeekExt;
use futures_lite::FutureExt;
use futures_lite::Stream;
use futures_util::pin_mut;
use futures_util::{StreamExt, TryStreamExt};
use std::io::Result;
use std::pin::Pin;
use std::task::{Context, Poll};

const READ_BUF_SIZE: usize = 1024 * 64;

pub async fn r() -> Result<()> {
    let entries = fs::read_dir(".").await?;

    let s = entries
        .map_ok(|d| messages::response::ls::Entry {
            name: d.file_name().into_string().unwrap(),
            size: 1000,
            is_dir: false,
        })
        .try_chunks(3);
    pin_mut!(s);
    while let Some(res) = s.next().await {
        println!("{:?}", res.unwrap());
        // println!("{}", entry.file_name().to_string_lossy());
    }
    Ok(())
}

pub struct ReadStream {
    file: fs::File,
    // read_buf: [u8; READ_BUF_SIZE],
}

impl ReadStream {
    pub async fn new(mut file: fs::File, start: Option<u64>, _end: Option<u64>) -> Result<Self> {
        // TODO implement `end`
        let start = match start {
            Some(s) => s,
            None => 0,
        };
        file.seek(std::io::SeekFrom::Start(start)).await?;

        Ok(ReadStream {
            file,
            // read_buf: [0; READ_BUF_SIZE],
        })
    }
}

impl Stream for ReadStream {
    type Item = response::Response;
    // type Item = Vec<u8>;
    fn poll_next(
        mut self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
    ) -> Poll<Option<<Self as Stream>::Item>> {
        // TODO we should not allocate on every call to poll
        let mut read_buf: [u8; READ_BUF_SIZE] = [0; READ_BUF_SIZE];
        match &Pin::new(&mut self.file).read(&mut read_buf).poll(ctx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(bytes_read) => match bytes_read {
                Ok(0) => return Poll::Ready(None),
                Err(_) => return Poll::Ready(None),
                Ok(b) => {
                    return Poll::Ready(Some(response::Response::Success(response::Success {
                        msg: Some(response::success::Msg::Read(response::Read {
                            data: read_buf[0..*b].to_vec(),
                        })),
                    })))
                }
            },
        }
    }
}

pub async fn read() -> Result<()> {
    let file = fs::File::open("Cargo.toml").await?;
    let mut rs = ReadStream::new(file, Some(5), None).await?;
    while let Some(msg) = rs.next().await {
        println!("Read msg {:?}", msg);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[async_std::test]
    async fn rt() {
        r().await.unwrap();
    }

    #[async_std::test]
    async fn test_read() {
        read().await.unwrap();
    }
}
