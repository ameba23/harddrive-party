use crate::{deserialize_message, messages, serialize_message};
use async_channel::{Receiver, Sender, TrySendError};
use futures_lite::io::{AsyncRead, AsyncWrite};
use futures_lite::ready;
use futures_lite::stream::Stream;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use thiserror::Error;

// TODO timeout
// TODO do we need a length prefix?

// This is a mock handshake implementation
pub fn create_handshake() -> messages::request::Msg {
    // TODO token should be random 32 bytes
    messages::request::Msg::Handshake(messages::request::Handshake {
        token: vec![10, 10, 10],
        version: None,
    })
}

pub fn create_handshake_response() -> messages::response::Response {
    messages::response::Response::Success(messages::response::Success {
        msg: Some(messages::response::success::Msg::Handshake(
            messages::response::Handshake {
                token: vec![10, 10, 10],
                version: None,
            },
        )),
    })
}

const READ_BUF_INITIAL_SIZE: usize = 1024 * 128;

/// A protocol event.
#[non_exhaustive]
#[derive(PartialEq, Debug)]
pub enum Event {
    HandshakeRequest,
    HandshakeResponse,
    Request(messages::request::Msg, u32),
    Response(messages::response::Response), // TODO do we use this?
    Responded,
}

#[derive(Debug)]
pub struct Options {
    pub is_initiator: bool,
}

impl Options {
    pub fn new(is_initiator: bool) -> Self {
        Self { is_initiator }
    }
}

#[derive(Debug)]
pub struct Protocol<IO> {
    io: IO,
    options: Options,
    pub handshaked: bool,
    outbound_rx: Receiver<Vec<u8>>,
    outbound_tx: Sender<Vec<u8>>,
    pub request_index: u32,
    open_requests: HashMap<u32, Sender<messages::response::Response>>,
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
            open_requests: HashMap::new(),
        }
    }

    /// Send a request and return a Receiver for the responses
    pub async fn request(
        &mut self,
        request: messages::request::Msg,
    ) -> Result<Receiver<messages::response::Response>, SendError> {
        // TODO error handle
        let (id, buf) = self.create_request(request);
        self.outbound_tx.send(buf).await?;
        let (tx, rx) = async_channel::unbounded();
        self.open_requests.insert(id, tx);
        Ok(rx)
    }

    /// Send a single response message relating to the given response id
    pub async fn respond(
        &mut self,
        response: messages::response::Response,
        id: u32,
    ) -> Result<(), SendError> {
        // TODO error handle
        let buf = self.create_response(id, response);
        self.outbound_tx.send(buf).await?;
        Ok(())
    }

    fn write_something(
        &mut self,
        cx: &mut Context<'_>,
        buf: Vec<u8>,
    ) -> Poll<Result<(), SendError>> {
        // println!("Writing {} bytes", buf.len());
        ready!(Pin::new(&mut self.io).poll_write(cx, &buf))?;
        Poll::Ready(Ok(()))
    }

    /// Poll for outbound messages and write them.
    fn poll_outbound_write(&mut self, cx: &mut Context<'_>) -> Result<(), SendError> {
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

    fn create_request(&mut self, message: messages::request::Msg) -> (u32, Vec<u8>) {
        let id = self.request_index;
        let message = messages::HdpMessage {
            id,
            msg: Some(messages::hdp_message::Msg::Request(messages::Request {
                msg: Some(message),
            })),
        };
        self.request_index += 1;
        (id, serialize_message(&message))
    }

    fn create_response(&mut self, id: u32, message: messages::response::Response) -> Vec<u8> {
        let message = messages::HdpMessage {
            id,
            msg: Some(messages::hdp_message::Msg::Response(messages::Response {
                response: Some(message),
            })),
        };
        serialize_message(&message)
    }
}

impl<IO> Stream for Protocol<IO>
where
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Item = Result<Event, SendError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut buf = vec![0u8; READ_BUF_INITIAL_SIZE as usize];

        match &Pin::new(&mut self.io).poll_read(cx, &mut buf) {
            Poll::Ready(Ok(n)) => {
                let message_buf = &buf[0..*n];
                let m = deserialize_message(message_buf).unwrap();
                match m.msg {
                    Some(messages::hdp_message::Msg::Request(messages::Request {
                        msg: Some(messages::request::Msg::Handshake(_h)),
                    })) => {
                        let message_id = m.id;
                        let message = self.create_response(message_id, create_handshake_response());
                        ready!(self.write_something(cx, message)).unwrap();
                        return Poll::Ready(Some(Ok(Event::HandshakeRequest)));
                    }
                    Some(messages::hdp_message::Msg::Response(messages::Response {
                        response:
                            Some(messages::response::Response::Success(messages::response::Success {
                                msg: Some(messages::response::success::Msg::Handshake(_h)),
                            })),
                    })) => {
                        self.handshaked = true;
                        return Poll::Ready(Some(Ok(Event::HandshakeResponse)));
                    }
                    Some(messages::hdp_message::Msg::Request(messages::Request {
                        msg: Some(r),
                    })) => {
                        println!("got a request {:?}", r);
                        println!("handshaked? {}", self.handshaked);
                        return Poll::Ready(Some(Ok(Event::Request(r, m.id))));
                    }
                    Some(messages::hdp_message::Msg::Response(messages::Response {
                        response: Some(r),
                    })) => {
                        println!("got a response {:?}", r);
                        println!("handshaked? {}", self.handshaked);
                        // TODO if the response is endResponse, close the channel (eg: with .drop())
                        match self.open_requests.get(&m.id) {
                            Some(sender) => match sender.try_send(r) {
                                Ok(()) => {
                                    println!("Sending response on channel");
                                    return Poll::Ready(Some(Ok(Event::Responded)));
                                }
                                Err(TrySendError::Full(_)) => {
                                    // Is this ever reachable with an unbounded channel?
                                    unreachable!("Channel full when trying to send response");
                                }
                                Err(TrySendError::Closed(_)) => {
                                    unreachable!("Channel sender closed")
                                }
                            },
                            None => {}
                        }
                        return Poll::Pending;
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
                // // If the reader is pending, poll the timeout.
                // Poll::Pending | Poll::Ready(Ok(_)) => {
                //     // Return Pending if the timeout is pending, or an error if the
                //     // timeout expired (i.e. returned Poll::Ready).
                //     return Pin::new(&mut self.timeout)
                //         .poll(cx)
                //         .map(|()| Err(Error::new(ErrorKind::TimedOut, "Remote timed out")));
            }
        };

        if !self.handshaked && self.options.is_initiator {
            let (_, message) = self.create_request(create_handshake());
            ready!(self.write_something(cx, message)).unwrap();
            self.handshaked = true;
            return Poll::Pending;
            // return Poll::Ready(Some(Ok(String::from("Initiating handshake"))));
        }

        // if self.handshaked {
        self.poll_outbound_write(cx)?;
        // };

        Poll::Pending
    }
}

/// Error when making a request or response
#[derive(Error, Debug)]
pub enum SendError {
    #[error(transparent)]
    IOError(#[from] async_channel::SendError<Vec<u8>>),
    // #[error("Cannot parse OsString")]
    // OsStringError(),
    // #[error("Unable to merge db record")]
    // DbMergeError(#[from] sled::Error),
    // #[error("Cannot get parent of given dir")]
    // GetParentError,
    // #[error("Got entry which does not appear to be a child of the given directory")]
    // PrefixError(#[from] std::path::StripPrefixError),
}
