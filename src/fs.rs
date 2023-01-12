use crate::messages::response;
use async_std::{fs, io::Read};
use futures::{AsyncSeekExt, Stream};
use std::io::Result;
use std::pin::Pin;
use std::task::{Context, Poll};

const READ_BUF_SIZE: usize = 1024 * 64;

pub struct ReadStream {
    file: fs::File,
    // read_buf: [u8; READ_BUF_SIZE],
    // end: u64
}

impl ReadStream {
    pub async fn new(mut file: fs::File, start: Option<u64>, _end: Option<u64>) -> Result<Self> {
        // TODO implement `end`
        let start = start.unwrap_or(0);
        file.seek(std::io::SeekFrom::Start(start)).await?;

        Ok(ReadStream {
            file,
            // read_buf: [0; READ_BUF_SIZE],
            // end
        })
    }
}

impl Stream for ReadStream {
    type Item = response::Response;

    fn poll_next(
        mut self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
    ) -> Poll<Option<<Self as Stream>::Item>> {
        // TODO we should not allocate on every call to poll
        let mut read_buf: [u8; READ_BUF_SIZE] = [0; READ_BUF_SIZE];
        // match &Pin::new(&mut self.file).read(&mut read_buf).poll(ctx) {
        match &Pin::new(&mut self.file).poll_read(ctx, &mut read_buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(bytes_read) => match bytes_read {
                Ok(0) => Poll::Ready(None),
                Err(_) => Poll::Ready(None), // TODO return an error response
                Ok(b) => Poll::Ready(Some(response::Response::Success(response::Success {
                    msg: Some(response::success::Msg::Read(response::Read {
                        data: read_buf[0..*b].to_vec(),
                    })),
                }))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;

    #[async_std::test]
    async fn test_read() {
        let file = fs::File::open("tests/test-data/somefile").await.unwrap();
        let mut rs = ReadStream::new(file, Some(2), None).await.unwrap();
        assert_eq!(
            Some(response::Response::Success(response::Success {
                msg: Some(response::success::Msg::Read(response::Read {
                    data: vec![111, 112, 10]
                }))
            })),
            rs.next().await
        );
        assert_eq!(None, rs.next().await);
        // while let Some(msg) = rs.next().await {
        //     println!("Read msg {:?}", msg);
        // }
    }
}
