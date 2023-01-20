use crate::messages;
use crate::messages::response::EndResponse;
use crate::run::IncomingPeerResponse;
use async_channel::{Receiver, Sender, TrySendError};
use futures::io::{AsyncRead, AsyncWrite};
use futures::{
    ready,
    stream::{FusedStream, Stream},
    StreamExt,
};
use log::{info, warn};
use prost::Message;
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use thiserror::Error;

// TODO timeout
// TODO do we need a length prefix?

const READ_BUF_INITIAL_SIZE: usize = 1024 * 128;
const PROTOCOL_VERSION: &str = "0";

/// A protocol event.
#[non_exhaustive]
#[derive(PartialEq, Debug)]
pub enum Event {
    HandshakeRequest,
    HandshakeResponse,
    Request(messages::request::Msg, u32),
    // Response(messages::response::Response), // TODO do we use this?
    Responded,
}

#[derive(Debug)]
pub struct Protocol<IO> {
    io: IO,
    pub is_initiator: bool,
    public_key: [u8; 32],
    outbound_rx: Receiver<Vec<u8>>,
    outbound_tx: Sender<Vec<u8>>,
    pub request_index: u32,
    open_requests: HashMap<u32, Sender<IncomingPeerResponse>>,
    /// Used for FusedStream trait
    is_terminated: bool,
    /// The remote peer's public key. This is only present after handshaking
    pub remote_pk: Option<[u8; 32]>,
}

impl<IO> Protocol<IO>
where
    IO: AsyncWrite + AsyncRead + Send + Unpin + 'static,
{
    /// Create a new protocol instance.
    pub fn new(io: IO, public_key: [u8; 32], is_initiator: bool) -> Self {
        info!("new protocol");
        let (outbound_tx, outbound_rx) = async_channel::unbounded();
        Protocol {
            io,
            is_initiator,
            public_key,
            remote_pk: None,
            outbound_tx,
            outbound_rx,
            request_index: 0,
            open_requests: HashMap::new(),
            is_terminated: false,
        }
    }

    /// Wait for the handshake before returning a new protocol instance
    pub async fn with_handshake(
        io: IO,
        public_key: [u8; 32],
        is_initiator: bool,
    ) -> Result<Self, HandshakeError> {
        let mut protocol = Protocol::new(io, public_key, is_initiator);
        let event = protocol.next().await;
        match event {
            Some(Ok(Event::HandshakeRequest)) => Ok(protocol),
            Some(Ok(Event::HandshakeResponse)) => Ok(protocol),
            _ => Err(HandshakeError::BadHandshake),
        }
    }

    /// Send a request and return a Receiver for the responses
    pub async fn request(
        &mut self,
        request: crate::run::OutGoingPeerRequest,
    ) -> Result<(), SendError> {
        info!("Making request {:?} {}", request, self.is_initiator);
        let (id, buf) = self.create_request(request.message);
        self.outbound_tx.send(buf).await?;
        self.open_requests.insert(id, request.response_tx);
        Ok(())
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

    fn write_bytes(
        &mut self,
        cx: &mut Context<'_>,
        buf: Vec<u8>,
    ) -> Poll<Result<(), PeerConnectionError>> {
        info!("Writing {} bytes", buf.len());
        ready!(Pin::new(&mut self.io).poll_write(cx, &buf))?;
        Poll::Ready(Ok(()))
    }

    /// Poll for outbound messages and write them.
    fn poll_outbound_write(&mut self, cx: &mut Context<'_>) -> Result<(), PeerConnectionError> {
        loop {
            // if let Poll::Ready(Err(e)) = self.write_state.poll_send(cx, &mut self.io) {
            //     return Err(e);
            // }
            // if !self.write_state.can_park_frame() || !matches!(self.state, State::Established) {
            //     return Ok(());
            // }

            match Pin::new(&mut self.outbound_rx).poll_next(cx) {
                Poll::Ready(Some(message)) => match self.write_bytes(cx, message) {
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
    type Item = Result<Event, PeerConnectionError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut buf = vec![0u8; READ_BUF_INITIAL_SIZE];
        match &Pin::new(&mut self.io).poll_read(cx, &mut buf) {
            Poll::Ready(Ok(n)) => {
                info!("Processing msg {}", n);
                let message_buf = &buf[0..*n];
                let m = deserialize_message(message_buf)?;
                match m.msg {
                    Some(messages::hdp_message::Msg::Request(messages::Request {
                        msg: Some(messages::request::Msg::Handshake(handshake_request)),
                    })) => {
                        info!("Got handshake request");

                        // Check protocol version number
                        if handshake_request.version != PROTOCOL_VERSION {
                            return Poll::Ready(Some(Err(
                                PeerConnectionError::VersionCompatibilityError,
                            )));
                        }

                        if let Ok(remote_pk) = handshake_request.token.try_into() {
                            self.remote_pk = Some(remote_pk);

                            // TODO replace with real handshake implementation
                            let pk = self.public_key;
                            let res = self.create_response(m.id, create_handshake_response(pk));
                            ready!(self.write_bytes(cx, res))?;
                            return Poll::Ready(Some(Ok(Event::HandshakeRequest)));
                        } else {
                            return Poll::Ready(Some(Err(PeerConnectionError::HandshakeError)));
                        };
                    }
                    Some(messages::hdp_message::Msg::Response(messages::Response {
                        response:
                            Some(messages::response::Response::Success(messages::response::Success {
                                msg:
                                    Some(messages::response::success::Msg::Handshake(
                                        handshake_response,
                                    )),
                            })),
                    })) => {
                        info!("Got handshake response");

                        // Check protocol version number
                        if handshake_response.version != PROTOCOL_VERSION {
                            return Poll::Ready(Some(Err(
                                PeerConnectionError::VersionCompatibilityError,
                            )));
                        }

                        // TODO replace with real handshake implementation
                        if let Ok(remote_pk) = handshake_response.token.try_into() {
                            self.remote_pk = Some(remote_pk);
                            return Poll::Ready(Some(Ok(Event::HandshakeResponse)));
                        } else {
                            return Poll::Ready(Some(Err(PeerConnectionError::HandshakeError)));
                        };
                    }
                    Some(messages::hdp_message::Msg::Request(messages::Request {
                        msg: Some(req),
                    })) => {
                        info!("Got a request {:?}", req);
                        return Poll::Ready(Some(if self.remote_pk.is_some() {
                            Ok(Event::Request(req, m.id))
                        } else {
                            warn!("Got a request before handshake completed");
                            Err(PeerConnectionError::BadMessageError)
                        }));
                    }
                    Some(messages::hdp_message::Msg::Response(messages::Response {
                        response: Some(res),
                    })) => {
                        info!("{} got a response {:?}", self.is_initiator, res);
                        if self.remote_pk.is_none() {
                            warn!("Got response message before handshake completed");
                            return Poll::Ready(Some(Err(PeerConnectionError::BadMessageError)));
                        }
                        match self.open_requests.get(&m.id) {
                            Some(sender) => {
                                // return Poll::Ready(Some(Ok(Event::Response(r))));
                                match res {
                                    crate::messages::response::Response::Success(
                                        messages::response::Success {
                                            msg:
                                                Some(messages::response::success::Msg::EndResponse(
                                                    EndResponse {},
                                                )),
                                        },
                                    ) => {
                                        sender.close();
                                    }
                                    _ => {
                                        let close_channel = matches!(
                                            res,
                                            crate::messages::response::Response::Err(_)
                                        );
                                        match sender.try_send(IncomingPeerResponse {
                                            message: res,
                                            public_key: self.remote_pk.unwrap(),
                                        }) {
                                            Ok(()) => {
                                                info!("Sent response on channel");
                                                if close_channel {
                                                    sender.close();
                                                };
                                                return Poll::Ready(Some(Ok(Event::Responded)));
                                            }
                                            Err(TrySendError::Full(_)) => {
                                                warn!("channel full");
                                                // Is this ever reachable with an unbounded channel?
                                                unreachable!(
                                                    "Channel full when trying to send response"
                                                );
                                            }
                                            Err(TrySendError::Closed(_)) => {
                                                warn!("Response channel closed");
                                                // TODO unreachable!("Channel sender closed")
                                                return Poll::Ready(Some(Ok(Event::Responded)));
                                            }
                                        }
                                    }
                                };
                            }
                            None => {
                                warn!("Cannot find associated request");
                                return Poll::Ready(Some(Err(
                                    PeerConnectionError::BadMessageError,
                                )));
                            }
                        }
                    }
                    _ => {
                        return Poll::Ready(Some(Err(PeerConnectionError::BadMessageError)));
                    }
                }
            }
            Poll::Ready(Err(_e)) => {
                self.is_terminated = true;
                return Poll::Ready(Some(Err(PeerConnectionError::ConnectionError)));
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

        if self.remote_pk.is_none() && self.is_initiator {
            let pk = self.public_key;
            let (_, message) = self.create_request(create_handshake(pk));
            ready!(self.write_bytes(cx, message))?;
            return Poll::Pending;
        }

        if self.remote_pk.is_some() {
            info!("Polling outbound write {}", self.is_initiator);
            self.poll_outbound_write(cx)?;
        } else {
            warn!(
                "Not polling outbound write, because handshake not made {}",
                self.is_initiator
            );
        };

        Poll::Pending
    }
}

impl<IO> FusedStream for Protocol<IO>
where
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    fn is_terminated(&self) -> bool {
        self.is_terminated
    }
}

fn serialize_message(message: &messages::HdpMessage) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.reserve(message.encoded_len());
    // Unwrap is safe, since we have reserved sufficient capacity in the vector.
    message.encode(&mut buf).unwrap();
    buf
}

fn deserialize_message(buf: &[u8]) -> Result<messages::HdpMessage, prost::DecodeError> {
    // messages::HdpMessage::decode(&mut Cursor::new(buf))
    messages::HdpMessage::decode(buf)
}

// This is a mock handshake implementation
// This will present a challenge to prove knowledge of swarm name
// But for now it is just used to give a public key
// In the real implementation, the public key will be established using a
// noise handshake
pub fn create_handshake(token: [u8; 32]) -> messages::request::Msg {
    // TODO token will be random 32 bytes
    messages::request::Msg::Handshake(messages::request::Handshake {
        token: Vec::from(token),
        version: PROTOCOL_VERSION.to_string(),
    })
}

pub fn create_handshake_response(token: [u8; 32]) -> messages::response::Response {
    messages::response::Response::Success(messages::response::Success {
        msg: Some(messages::response::success::Msg::Handshake(
            messages::response::Handshake {
                token: Vec::from(token),
                version: PROTOCOL_VERSION.to_string(),
            },
        )),
    })
}

/// Error when handshaking
#[derive(Error, Debug)]
pub enum HandshakeError {
    // TODO this will be more elaborate
    #[error("Error making handshake")]
    BadHandshake,
}

/// Error when communicating with peer
#[derive(Error, Debug)]
pub enum PeerConnectionError {
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    #[error("Connection error")]
    ConnectionError,
    #[error("Failed to handle message from peer")]
    BadMessageError,
    #[error(transparent)]
    DeserializationError(#[from] prost::DecodeError),
    #[error("Handshake error")]
    HandshakeError,
    #[error("Remote peer has incompatible protocol version")]
    VersionCompatibilityError,
}

/// Error when making a request or response
#[derive(Error, Debug)]
pub enum SendError {
    #[error(transparent)]
    ChannelWriteError(#[from] async_channel::SendError<Vec<u8>>),
    #[error("Error writing")]
    WriteError,
}
