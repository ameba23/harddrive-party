use async_channel::{Receiver, Sender};
use futures_lite::io::{AsyncRead, AsyncWrite};
use futures_lite::ready;
use futures_lite::stream::Stream;
use prost::Message;
use std::io::{Error, ErrorKind, Result};
use std::pin::Pin;
use std::result::Result as ResultResult;
use std::task::{Context, Poll};

pub mod messages {
    include!(concat!(env!("OUT_DIR"), "/harddriveparty.messages.rs"));
}

pub fn serialize_message(message: &messages::HdpMessage) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.reserve(message.encoded_len());
    // Unwrap is safe, since we have reserved sufficient capacity in the vector.
    message.encode(&mut buf).unwrap();
    buf
}

pub fn deserialize_message(buf: &[u8]) -> ResultResult<messages::HdpMessage, prost::DecodeError> {
    // messages::HdpMessage::decode(&mut Cursor::new(buf))
    messages::HdpMessage::decode(buf)
}

pub fn create_handshake() -> messages::request::Msg {
    // TODO token should be random 32 bytes
    messages::request::Msg::Handshake(messages::request::Handshake {
        token: vec![10, 10, 10],
        version: None,
    })
}

pub fn create_handshake_response() -> messages::Response {
    messages::Response {
        response: Some(messages::response::Response::Success(
            messages::response::Success {
                msg: Some(messages::response::success::Msg::Handshake(
                    messages::response::Handshake {
                        token: vec![10, 10, 10],
                        version: None,
                    },
                )),
            },
        )),
    }
}

const READ_BUF_INITIAL_SIZE: usize = 1024 * 128;

/// A protocol event.
#[non_exhaustive]
#[derive(PartialEq, Debug)]
pub enum Event {
    HandshakeRequest,
    HandshakeResponse,
    Request(messages::request::Msg),
}

#[derive(Debug)]
pub struct Options {
    pub is_initiator: bool,
}

impl Options {
    /// Create with default options.
    pub fn new(is_initiator: bool) -> Self {
        Self { is_initiator }
    }
}

#[derive(Debug)]
pub struct Protocol<IO> {
    io: IO,
    options: Options,
    handshaked: bool,
    outbound_rx: Receiver<Vec<u8>>,
    outbound_tx: Sender<Vec<u8>>,
    request_index: u32,
}

impl<IO> Protocol<IO>
where
    IO: AsyncWrite + AsyncRead + Send + Unpin + 'static,
{
    /// Create a new protocol instance.
    pub fn new(io: IO, options: Options) -> Self {
        let (outbound_tx, outbound_rx) = async_channel::bounded(1);
        Protocol {
            io,
            options,
            handshaked: false,
            outbound_tx,
            outbound_rx,
            request_index: 0,
        }
    }

    fn write_something(&mut self, cx: &mut Context<'_>, buf: Vec<u8>) -> Poll<Result<()>> {
        ready!(Pin::new(&mut self.io).poll_write(cx, &buf))?;
        Poll::Ready(Ok(()))
    }

    pub async fn request(&mut self, request: messages::request::Msg) -> Result<()> {
        let buf = self.create_request(request);
        // write msg to outbound tx
        self.outbound_tx.send(buf).await.unwrap();
        Ok(())
    }

    /// Poll for outbound messages and write them.
    fn poll_outbound_write(&mut self, cx: &mut Context<'_>) -> Result<()> {
        loop {
            // if let Poll::Ready(Err(e)) = self.write_state.poll_send(cx, &mut self.io) {
            //     return Err(e);
            // }
            // if !self.write_state.can_park_frame() || !matches!(self.state, State::Established) {
            //     return Ok(());
            // }

            match Pin::new(&mut self.outbound_rx).poll_next(cx) {
                Poll::Ready(Some(message)) => match self.write_something(cx, message) {
                    Poll::Ready(_) => {}
                    Poll::Pending => return Ok(()),
                },
                Poll::Ready(None) => unreachable!("Channel closed before end"),
                Poll::Pending => return Ok(()),
            }
        }
    }

    fn create_request(&mut self, message: messages::request::Msg) -> Vec<u8> {
        // messages::hdp_message::Msg::Request(messages::Request {
        let message = messages::HdpMessage {
            id: self.request_index,
            msg: Some(messages::hdp_message::Msg::Request(messages::Request {
                msg: Some(message),
            })),
        };
        self.request_index += 1;
        serialize_message(&message)
    }

    fn create_response(&mut self, message: messages::Response) -> Vec<u8> {
        let message = messages::HdpMessage {
            id: self.request_index,
            msg: Some(messages::hdp_message::Msg::Response(message)),
        };
        serialize_message(&message)
    }
}

impl<IO> Stream for Protocol<IO>
where
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Item = Result<Event>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut buf = vec![0u8; READ_BUF_INITIAL_SIZE as usize];

        match &Pin::new(&mut self.io).poll_read(cx, &mut buf) {
            Poll::Ready(Ok(n)) => {
                let message_buf = &buf[0..*n];
                let m = deserialize_message(message_buf)?;
                match m.msg {
                    Some(messages::hdp_message::Msg::Request(messages::Request {
                        msg: Some(messages::request::Msg::Handshake(_h)),
                    })) => {
                        let message = self.create_response(create_handshake_response());
                        ready!(self.write_something(cx, message)).unwrap();
                        return Poll::Ready(Some(Ok(Event::HandshakeRequest)));
                    }
                    Some(messages::hdp_message::Msg::Response(messages::Response {
                        response:
                            Some(messages::response::Response::Success(messages::response::Success {
                                msg: Some(messages::response::success::Msg::Handshake(_h)),
                            })),
                    })) => {
                        return Poll::Ready(Some(Ok(Event::HandshakeResponse)));
                    }
                    Some(messages::hdp_message::Msg::Request(messages::Request {
                        msg: Some(r),
                    })) => {
                        println!("got {:?}", r);
                        println!("handshaked? {}", self.handshaked);
                        return Poll::Ready(Some(Ok(Event::Request(r))));
                    }
                    _ => {
                        return Poll::Ready(Some(Err(Error::new(
                            ErrorKind::Other,
                            format!("Got unkown message type {:?}", m.msg),
                        ))));
                    }
                }
            }
            Poll::Ready(Err(_e)) => {
                return Poll::Ready(Some(Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "e",
                ))))
            }
            Poll::Pending => {
                println!("ppp");
            } // Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
              // // If the reader is pending, poll the timeout.
              // Poll::Pending | Poll::Ready(Ok(_)) => {
              //     // Return Pending if the timeout is pending, or an error if the
              //     // timeout expired (i.e. returned Poll::Ready).
              //     return Pin::new(&mut self.timeout)
              //         .poll(cx)
              //         .map(|()| Err(Error::new(ErrorKind::TimedOut, "Remote timed out")));
              // }
        };

        if !self.handshaked && self.options.is_initiator {
            let message = self.create_request(create_handshake());
            ready!(self.write_something(cx, message)).unwrap();
            self.handshaked = true;
            return Poll::Pending;
            // return Poll::Ready(Some(Ok(String::from("Initiating handshake"))));
        }

        if self.handshaked {
            self.poll_outbound_write(cx)?;
        };

        Poll::Pending
    }
}
