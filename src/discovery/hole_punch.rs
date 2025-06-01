//! Udp hole-punch logic
//! A lot of this is copied from <https://github.com/Frando/quinn-holepunch>
use futures::ready;
use log::{debug, info, warn};
use rand::{Rng, SeedableRng};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::{mpsc as std_mpsc, Arc},
    task::{Context, Poll},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::Interest,
    net::UdpSocket,
    select, spawn,
    sync::{broadcast, mpsc, RwLock},
};

pub type UdpReceive = broadcast::Receiver<IncomingHolepunchPacket>;
pub type UdpSend = mpsc::Sender<OutgoingHolepunchPacket>;

const MAX_HOLEPUNCH_ATTEMPTS: usize = 50;
const MAX_UNKNOWN_PORT_HOLEPUNCH_ATTEMPTS: usize = 2048;
const PACKET_CHANNEL_CAPACITY: usize = 1024;
const HOLEPUNCH_WAIT_MILLIS: u64 = 2000;

/// UDP socket which sends holepunch packets using the [HolePuncher]
#[derive(Debug)]
pub struct PunchingUdpSocket {
    /// The underlying UDP socket
    socket: Arc<tokio::net::UdpSocket>,
    quinn_socket_state: Arc<quinn_udp::UdpSocketState>,
    udp_recv_tx: broadcast::Sender<IncomingHolepunchPacket>,
}

impl PunchingUdpSocket {
    /// Given a raw UDP socket, return a [PunchingUdpSocket] and a [HolePuncher] which allows us to
    /// send holepunch packets on the socket
    pub async fn bind(socket: tokio::net::UdpSocket) -> io::Result<(Self, HolePuncher)> {
        let socket = socket.into_std()?;

        quinn_udp::UdpSocketState::configure((&socket).into())?;

        let socket = Arc::new(tokio::net::UdpSocket::from_std(socket)?);

        let (udp_recv_tx, _udp_recv) =
            broadcast::channel::<IncomingHolepunchPacket>(PACKET_CHANNEL_CAPACITY);
        let (udp_send, mut udp_send_rx) =
            mpsc::channel::<OutgoingHolepunchPacket>(PACKET_CHANNEL_CAPACITY);

        // Loop over outgoing packets from the HolePuncher
        let socket_clone = socket.clone();
        let mut rng = rand::thread_rng();
        // TODO this could be a bigger seed (eg: [u8; 32])
        let rng_seed: u64 = rng.gen();
        tokio::spawn(async move {
            while let Some(packet) = udp_send_rx.recv().await {
                match socket_clone.send_to(&packet.data, packet.dest).await {
                    Ok(_) => {}
                    Err(err) => warn!(
                        "Failed to send holepunch packet to {}: {}",
                        packet.dest, err
                    ),
                }
            }
        });

        Ok((
            Self {
                socket,
                quinn_socket_state: Arc::new(quinn_udp::UdpSocketState::new()),
                udp_recv_tx: udp_recv_tx.clone(),
            },
            HolePuncher {
                udp_send,
                udp_recv_tx,
                rng_seed,
            },
        ))
    }
}

impl quinn::AsyncUdpSocket for PunchingUdpSocket {
    fn poll_send(
        &self,
        state: &quinn_udp::UdpState,
        cx: &mut Context,
        transmits: &[quinn_udp::Transmit],
    ) -> Poll<io::Result<usize>> {
        let quinn_socket_state = &*self.quinn_socket_state;
        let io = &*self.socket;
        loop {
            ready!(io.poll_send_ready(cx))?;
            if let Ok(res) = io.try_io(Interest::WRITABLE, || {
                quinn_socket_state.send(io.into(), state, transmits)
            }) {
                return Poll::Ready(Ok(res));
            }
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [std::io::IoSliceMut<'_>],
        metas: &mut [quinn_udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            ready!(self.socket.poll_recv_ready(cx))?;
            if let Ok(res) = self.socket.try_io(Interest::READABLE, || {
                let res = self
                    .quinn_socket_state
                    .recv((&*self.socket).into(), bufs, metas);

                if let Ok(msg_count) = res {
                    forward_holepunch(&self.udp_recv_tx, bufs, metas, msg_count);
                }

                res
            }) {
                return Poll::Ready(Ok(res));
            }
        }
    }

    fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.socket.local_addr()
    }
}

fn forward_holepunch(
    channel: &broadcast::Sender<IncomingHolepunchPacket>,
    bufs: &[std::io::IoSliceMut<'_>],
    metas: &[quinn_udp::RecvMeta],
    msg_count: usize,
) {
    for (meta, buf) in metas.iter().zip(bufs.iter()).take(msg_count) {
        if meta.len == 1 {
            let packet = IncomingHolepunchPacket {
                data: [buf[0]], // *&buf[0]
                from: meta.addr,
            };
            debug!("Forwarding hole punch packet");
            let _ = channel.send(packet);
        }
    }
}

/// A holepunch packet we have received
#[derive(Clone, Debug)]
pub struct IncomingHolepunchPacket {
    /// This is one u8, where 0 is the initiator packet, and 1 is an acknowledgement packet
    data: [u8; 1],
    from: SocketAddr,
}

/// A holepunch packet we want to send
#[derive(Clone, Debug)]
pub struct OutgoingHolepunchPacket {
    data: [u8; 1],
    dest: SocketAddr,
}

impl OutgoingHolepunchPacket {
    fn new_init(dest: SocketAddr) -> Self {
        Self { data: [0u8], dest }
    }

    fn new_ack(dest: SocketAddr) -> Self {
        Self { data: [1u8], dest }
    }
}

/// Handles requests to connect to a peer by holepunching using a channel to a [PunchingUdpSocket]
#[derive(Clone, Debug)]
pub struct HolePuncher {
    udp_send: UdpSend,
    udp_recv_tx: broadcast::Sender<IncomingHolepunchPacket>,
    rng_seed: u64,
}

impl HolePuncher {
    /// Make a connection by holepunching
    pub async fn hole_punch_peer(&mut self, addr: SocketAddr) -> Result<(), HolePunchError> {
        let mut udp_recv = self.udp_recv_tx.subscribe();
        let mut packet = OutgoingHolepunchPacket {
            dest: addr,
            data: [0u8],
        };
        let mut wait = false;
        let mut sent_ack = false;
        let mut received_ack = false;
        let mut attempts = 0;
        loop {
            if wait {
                tokio::time::sleep(Duration::from_millis(HOLEPUNCH_WAIT_MILLIS)).await;
            }
            tokio::select! {
              send = self.udp_send.send(packet.clone()) => {
                  if let Err(err) = send {
                      warn!("Failed to forward holepunch packet to {addr}: {err}");
                  } else if packet.data == [0u8] {
                      debug!("sent initial packet to {addr}, waiting");
                      attempts += 1;
                      if attempts >= MAX_HOLEPUNCH_ATTEMPTS {
                          return Err(HolePunchError::MaxAttempts);
                      }
                  } else {
                      debug!("sent ack packet to {addr}, waiting");
                      sent_ack = true;
                      if received_ack {
                          break
                      };
                  }
                  wait = true;
              }
              recv = udp_recv.recv() => {
                  if let Ok(recv) = recv {
                      if recv.from == addr {
                          match recv.data[0] {
                              0 => {
                                  debug!("Received initial holepunch packet from {addr}");
                                  packet.data = [1u8];
                              }
                              1 => {
                                  debug!("Received ack holepunch packet from {addr}");
                                  packet.data = [1u8];
                                  received_ack = true;
                                  if sent_ack {
                                      break
                                  };
                              },
                              _ => warn!("Received invalid holepunch packet from {addr}")
                          }
                      } else {
                          debug!("Received unrelated packet from {}", recv.from);
                      }
                  }
                  wait = false;
              }
            }
        }
        Ok(())
    }

    /// Hole punch to a peer for who we do not know which port
    pub async fn hole_punch_peer_without_port(
        &mut self,
        addr: IpAddr,
    ) -> Result<SocketAddr, HolePunchError> {
        let mut udp_recv = self.udp_recv_tx.subscribe();

        let stop_signal = Arc::new(RwLock::new(false));
        let stop_clone = stop_signal.clone();

        let udp_send = self.udp_send.clone();
        let seed = self.rng_seed;
        let join_handle = tokio::spawn(async move {
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            for _ in 0..MAX_UNKNOWN_PORT_HOLEPUNCH_ATTEMPTS {
                // Send a packet to a random port
                let packet = OutgoingHolepunchPacket::new_init(SocketAddr::new(addr, rng.gen()));
                if let Err(err) = udp_send.send(packet).await {
                    warn!("Failed to forward holepunch packet to {addr}: {err}");
                }

                if *stop_clone.read().await {
                    break;
                }
            }
            debug!("Stopped sending");
        });

        // Wait for a response
        // TODO we should have a timeout here
        let sender = loop {
            let recv = udp_recv.recv().await?;
            // TODO should check the message is a hole punch packet
            if recv.from.ip() == addr {
                break recv.from;
            }
            debug!("Got message from unexpected sender - ignoring");
        };

        // Signal to stop trying connections
        {
            let mut stop = stop_signal.write().await;
            *stop = true;
        }
        join_handle.await?;

        // Send a packet back
        self.udp_send
            .send(OutgoingHolepunchPacket::new_ack(sender))
            .await?;
        Ok(sender)
    }

    pub fn spawn_hole_punch_peer(&mut self, addr: SocketAddr) {
        let mut hole_puncher_clone = self.clone();
        tokio::spawn(async move {
            info!("Attempting hole punch...");
            if hole_puncher_clone.hole_punch_peer(addr).await.is_err() {
                warn!("Hole punching failed");
            } else {
                info!("Hole punching succeeded");
            };
        });
    }
}

/// The number of sockets to listen on
const INCOMING_SOCKETS: usize = 256;
/// The inital holepunch packet sent
const INIT_PUNCH: [u8; 1] = [0];
/// The acknowledgement holepunch packet
const ACK_PUNCH: [u8; 1] = [1];

pub async fn birthday_hard_side(
    target_addr: SocketAddr,
) -> Result<(UdpSocket, SocketAddr), HolePunchError> {
    let (socket_tx, socket_rx) = std_mpsc::channel(); // TODO limit
    let (stop_signal_tx, _stop_signal_rx) = broadcast::channel(1);

    for _ in 0..INCOMING_SOCKETS {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let socket_tx = socket_tx.clone();
        let mut stop_signal_rx = stop_signal_tx.subscribe();
        spawn(async move {
            if let Err(error) = socket.send_to(&INIT_PUNCH, target_addr).await {
                warn!("Send error: {:?}", error);
            }

            let mut buf = [0u8; 32];
            select! {
                recv_result = socket.recv_from(&mut buf) => {
                    match recv_result {
                        Ok((_len, sender)) => {
                            if sender == target_addr {
                                if let Err(error) = socket.send_to(&ACK_PUNCH, sender).await {
                                    warn!("Send error: {:?}", error);
                                }
                                if socket_tx.send(socket).is_err() {
                                    // This may happen if we get more than one successful punch
                                    warn!("Cannot send success socket - channel closed");
                                }
                            } else {
                                warn!("Got message from unexpected sender {}", sender);
                            }
                        }
                        Err(error) => {
                            warn!("Cannot recieve on socket {:?} - {:?}", socket, error);
                        }
                    }
                }
                stop_signal_result = stop_signal_rx.recv() => {
                    debug!("Stop signal result {:?}", stop_signal_result);
                }
            }
        });
    }
    // TODO we should have a timeout here
    let socket = socket_rx.recv()?;
    // This stops all the listener tasks
    stop_signal_tx
        .send(true)
        .map_err(|_| HolePunchError::Broadcast)?;
    Ok((socket, target_addr))
}

#[derive(Error, Debug)]
pub enum HolePunchError {
    #[error("MPSC recv: {0}")]
    MpSc(#[from] std::sync::mpsc::RecvError),
    #[error("Could not send stop signal due to broadcaster error")]
    Broadcast,
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("Reached max holepunch attempts - giving up")]
    MaxAttempts,
    #[error("Broadcast receive: {0}")]
    BroadCastRecv(#[from] tokio::sync::broadcast::error::RecvError),
    #[error("Panic during task which sends outgoing packets")]
    Join(#[from] tokio::task::JoinError),
    #[error("Could not send holepunch packet - channel closed")]
    MpscSend(#[from] tokio::sync::mpsc::error::SendError<OutgoingHolepunchPacket>),
}
