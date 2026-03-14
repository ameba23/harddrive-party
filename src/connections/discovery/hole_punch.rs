//! Udp hole-punch logic
//! A lot of this is copied from <https://github.com/Frando/quinn-holepunch>
use futures::ready;
use log::{debug, info, warn};
use rand::{rngs::OsRng, Rng, RngCore, SeedableRng};
use std::{
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::Interest,
    net::UdpSocket,
    select, spawn,
    sync::{broadcast, mpsc, oneshot, RwLock},
};

pub type UdpReceive = broadcast::Receiver<IncomingHolepunchPacket>;
pub type UdpSend = mpsc::Sender<OutgoingHolepunchRequest>;

/// How many attempts to holepunch
const MAX_HOLEPUNCH_ATTEMPTS: usize = 50;

/// How long to wait between holepunch attempts (milliseconds)
const HOLEPUNCH_WAIT_MILLIS: u64 = 2000;

/// How many attempts to holepunch when guessing the remote port
const MAX_UNKNOWN_PORT_HOLEPUNCH_ATTEMPTS: usize = 2048;

/// Maximum time to wait for a hole-punch response
const HOLEPUNCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum capacity of channel used to send/recieve holepunch pakets
const PACKET_CHANNEL_CAPACITY: usize = 1024;

/// The number of sockets to listen on
const UNKNOWN_PORT_INCOMING_SOCKETS: usize = 256;

/// The inital holepunch packet sent
const INIT_PUNCH: [u8; 1] = [0];

/// The acknowledgement holepunch packet
const ACK_PUNCH: [u8; 1] = [1];

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

        quinn_udp::UdpSocketState::new((&socket).into())?;

        let socket = Arc::new(tokio::net::UdpSocket::from_std(socket)?);

        let (udp_recv_tx, _udp_recv) =
            broadcast::channel::<IncomingHolepunchPacket>(PACKET_CHANNEL_CAPACITY);
        let (udp_send, mut udp_send_rx) =
            mpsc::channel::<OutgoingHolepunchRequest>(PACKET_CHANNEL_CAPACITY);

        // Loop over outgoing packets from the HolePuncher
        let socket_clone = socket.clone();
        let rng_seed = next_rng_seed();
        tokio::spawn(async move {
            while let Some(request) = udp_send_rx.recv().await {
                let result = socket_clone
                    .send_to(&request.packet.data, request.packet.dest)
                    .await
                    .map(|_| ());
                if let Err(err) = &result {
                    warn!(
                        "Failed to send holepunch packet to {}: {}",
                        request.packet.dest, err
                    );
                }
                let _ = request.result_tx.send(result);
            }
        });

        Ok((
            Self {
                socket: socket.clone(),
                quinn_socket_state: Arc::new(quinn_udp::UdpSocketState::new((&socket).into())?),
                udp_recv_tx: udp_recv_tx.clone(),
            },
            HolePuncher {
                udp_send,
                udp_recv_tx,
                rng_seed,
            },
        ))
    }

    pub fn get_port(&self) -> anyhow::Result<u16> {
        Ok(self.socket.local_addr()?.port())
    }
}

#[derive(Debug)]
struct PunchingUdpPoller {
    inner: Arc<PunchingUdpSocket>,
}

impl quinn::UdpPoller for PunchingUdpPoller {
    fn poll_writable(
        self: Pin<&mut PunchingUdpPoller>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        // Register the QUIC stack's Waker with the tokio socket for write readiness
        // We call poll_send_ready on the inner tokio socket
        self.inner.socket.poll_send_ready(cx)
    }
}

impl quinn::AsyncUdpSocket for PunchingUdpSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn quinn::UdpPoller>> {
        Box::pin(PunchingUdpPoller { inner: self })
    }

    fn try_send(&self, transmit: &quinn_udp::Transmit<'_>) -> io::Result<()> {
        // NOTE: tokio::net::UdpSocket does not directly support QUIC's multi-segment
        // Transmit (which is for GSO/GRO). For simplicity and standard usage, we
        // assume a single datagram send.

        let sent_bytes = self
            .socket
            .try_send_to(transmit.contents, transmit.destination)?;

        if sent_bytes == transmit.contents.len() {
            Ok(())
        } else {
            // This case should generally not happen for UDP unless the datagram was
            // partially sent, which is usually not possible with a single UDP send.
            // If it were a stream, a partial write would be normal.
            Err(io::Error::other(
                "Partial UDP datagram send detected, which is unexpected.",
            ))
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

#[derive(Debug)]
pub struct OutgoingHolepunchRequest {
    packet: OutgoingHolepunchPacket,
    result_tx: oneshot::Sender<io::Result<()>>,
}

impl OutgoingHolepunchPacket {
    fn new_init(dest: SocketAddr) -> Self {
        Self {
            data: INIT_PUNCH,
            dest,
        }
    }

    fn new_ack(dest: SocketAddr) -> Self {
        Self {
            data: ACK_PUNCH,
            dest,
        }
    }
}

fn random_nonzero_port(rng: &mut impl Rng) -> u16 {
    rng.gen_range(1..=u16::MAX)
}

fn next_rng_seed() -> [u8; 32] {
    let mut rng_seed = [0u8; 32];
    OsRng.fill_bytes(&mut rng_seed);
    rng_seed
}

/// Handles requests to connect to a peer by holepunching using a channel to a [PunchingUdpSocket]
#[derive(Debug)]
pub struct HolePuncher {
    udp_send: UdpSend,
    udp_recv_tx: broadcast::Sender<IncomingHolepunchPacket>,
    rng_seed: [u8; 32],
}

impl Clone for HolePuncher {
    fn clone(&self) -> Self {
        Self {
            udp_send: self.udp_send.clone(),
            udp_recv_tx: self.udp_recv_tx.clone(),
            rng_seed: next_rng_seed(),
        }
    }
}

async fn send_holepunch_packet(
    udp_send: &UdpSend,
    packet: OutgoingHolepunchPacket,
) -> Result<(), HolePunchError> {
    let dest = packet.dest;
    let (result_tx, result_rx) = oneshot::channel();
    udp_send
        .send(OutgoingHolepunchRequest { packet, result_tx })
        .await
        .map_err(|_| HolePunchError::ChannelClosed)?;

    match result_rx.await {
        Ok(result) => result.map_err(HolePunchError::from),
        Err(_) => Err(HolePunchError::SendTaskClosed(dest)),
    }
}

impl HolePuncher {
    /// Make a connection by holepunching
    pub async fn hole_punch_peer(&mut self, addr: SocketAddr) -> Result<(), HolePunchError> {
        info!(
            "Hole punch started: mode=known-port remote_addr={} timeout_secs={} attempt_limit={} wait_ms={}",
            addr,
            HOLEPUNCH_TIMEOUT.as_secs(),
            MAX_HOLEPUNCH_ATTEMPTS,
            HOLEPUNCH_WAIT_MILLIS
        );
        tokio::time::timeout(HOLEPUNCH_TIMEOUT, async {
            let mut udp_recv = self.udp_recv_tx.subscribe();
            let mut packet = OutgoingHolepunchPacket::new_init(addr);
            let mut wait = false;
            let mut sent_ack = false;
            let mut received_ack = false;
            let mut attempts = 0;
            loop {
                if wait {
                    tokio::time::sleep(Duration::from_millis(HOLEPUNCH_WAIT_MILLIS)).await;
                }
                tokio::select! {
                  send_result = send_holepunch_packet(&self.udp_send, packet.clone()) => {
                      send_result?;
                      if packet.data == INIT_PUNCH {
                          attempts += 1;
                          debug!(
                              "Hole punch attempt sent: mode=known-port remote_addr={} attempt={}/{}",
                              addr,
                              attempts,
                              MAX_HOLEPUNCH_ATTEMPTS
                          );
                          if attempts >= MAX_HOLEPUNCH_ATTEMPTS {
                              return Err(HolePunchError::MaxAttempts);
                          }
                      } else {
                          debug!("Hole punch ack sent: mode=known-port remote_addr={addr}");
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
                                      debug!("Received initial hole punch packet from {addr}");
                                      packet.data = [1u8];
                                  }
                                  1 => {
                                      debug!("Received ack hole punch packet from {addr}");
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
        })
        .await
        .map_err(|_| HolePunchError::Timeout)
        .and_then(|result| result)
        .inspect(|_| info!("Hole punch succeeded: mode=known-port remote_addr={addr}"))
        .inspect_err(|err| warn!("Hole punch failed: mode=known-port remote_addr={} error={}", addr, err))
    }

    /// Hole punch to a peer for who we do not know which port
    pub async fn hole_punch_peer_without_port(
        &mut self,
        addr: IpAddr,
    ) -> Result<SocketAddr, HolePunchError> {
        info!(
            "Hole punch started: mode=unknown-port remote_ip={} timeout_secs={} probe_limit={}",
            addr,
            HOLEPUNCH_TIMEOUT.as_secs(),
            MAX_UNKNOWN_PORT_HOLEPUNCH_ATTEMPTS
        );
        let mut udp_recv = self.udp_recv_tx.subscribe();

        let stop_signal = Arc::new(RwLock::new(false));
        let stop_clone = stop_signal.clone();

        let udp_send = self.udp_send.clone();
        let seed = self.rng_seed;
        let join_handle = tokio::spawn(async move {
            let mut rng = rand::rngs::StdRng::from_seed(seed);
            for attempt in 1..=MAX_UNKNOWN_PORT_HOLEPUNCH_ATTEMPTS {
                // Send a packet to a random port
                let port = random_nonzero_port(&mut rng);
                let packet = OutgoingHolepunchPacket::new_init(SocketAddr::new(
                    addr,
                    port,
                ));
                debug!(
                    "Hole punch probe sent: mode=unknown-port remote_ip={} attempt={}/{} port={}",
                    addr,
                    attempt,
                    MAX_UNKNOWN_PORT_HOLEPUNCH_ATTEMPTS,
                    port
                );
                if let Err(err) = send_holepunch_packet(&udp_send, packet).await {
                    warn!("Failed to send holepunch packet to {addr}: {err}");
                    return Err(err);
                }

                if *stop_clone.read().await {
                    break;
                }
            }
            debug!("Stopped sending");
            Ok::<(), HolePunchError>(())
        });

        // Wait for a response
        let sender = tokio::time::timeout(HOLEPUNCH_TIMEOUT, async {
            loop {
                let recv = udp_recv.recv().await?;
                if recv.from.ip() == addr && matches!(recv.data[0], 0 | 1) {
                    break Ok::<SocketAddr, HolePunchError>(recv.from);
                }
                debug!("Got message from unexpected sender - ignoring");
            }
        })
        .await
        .map_err(|_| HolePunchError::Timeout)??;

        // Signal to stop trying connections
        {
            let mut stop = stop_signal.write().await;
            *stop = true;
        }
        join_handle.await??;
        info!(
            "Hole punch probe received response: mode=unknown-port remote_ip={} sender={}",
            addr, sender
        );

        // Send a packet back
        send_holepunch_packet(&self.udp_send, OutgoingHolepunchPacket::new_ack(sender)).await?;
        info!(
            "Hole punch succeeded: mode=unknown-port remote_ip={} sender={}",
            addr, sender
        );
        Ok(sender)
    }

    pub fn spawn_hole_punch_peer(&mut self, addr: SocketAddr) {
        let mut hole_puncher_clone = self.clone();
        tokio::spawn(async move {
            info!("Attempting hole punch...");
            if let Err(err) = hole_puncher_clone.hole_punch_peer(addr).await {
                warn!("Hole punching failed: {err}");
            } else {
                info!("Hole punching succeeded");
            };
        });
    }
}

pub async fn birthday_hard_side(
    target_addr: SocketAddr,
) -> Result<(UdpSocket, SocketAddr), HolePunchError> {
    info!(
        "Birthday hard side started: target_addr={} sockets={} timeout_secs={}",
        target_addr,
        UNKNOWN_PORT_INCOMING_SOCKETS,
        HOLEPUNCH_TIMEOUT.as_secs()
    );
    let (socket_tx, mut socket_rx) = mpsc::channel(1);
    let (stop_signal_tx, _stop_signal_rx) = broadcast::channel(1);

    for _ in 0..UNKNOWN_PORT_INCOMING_SOCKETS {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let socket_tx = socket_tx.clone();
        let mut stop_signal_rx = stop_signal_tx.subscribe();
        spawn(async move {
            if let Err(error) = socket.send_to(&INIT_PUNCH, target_addr).await {
                warn!("Send error: {error:?}");
            }

            let mut buf = [0u8; 32];
            select! {
                recv_result = socket.recv_from(&mut buf) => {
                    match recv_result {
                        Ok((_len, sender)) => {
                            if sender == target_addr {
                                if let Err(error) = socket.send_to(&ACK_PUNCH, sender).await {
                                    warn!("Send error: {error:?}");
                                }
                                if socket_tx.send(socket).await.is_err() {
                                    // This may happen if we get more than one successful punch
                                    warn!("Cannot send success socket - channel closed");
                                }
                            } else {
                                warn!("Got message from unexpected sender {sender}");
                            }
                        }
                        Err(error) => {
                            warn!("Cannot recieve on socket {socket:?} - {error:?}");
                        }
                    }
                }
                stop_signal_result = stop_signal_rx.recv() => {
                    debug!("Stop signal result {stop_signal_result:?}");
                }
            }
        });
    }
    drop(socket_tx);
    let socket = tokio::time::timeout(HOLEPUNCH_TIMEOUT, socket_rx.recv())
        .await
        .map_err(|_| HolePunchError::Timeout)?
        .ok_or(HolePunchError::SocketChannelClosed)?;
    // This stops all the listener tasks
    stop_signal_tx
        .send(true)
        .map_err(|_| HolePunchError::Broadcast)?;
    info!("Birthday hard side succeeded: target_addr={}", target_addr);
    Ok((socket, target_addr))
}

#[derive(Error, Debug)]
pub enum HolePunchError {
    #[error("Could not send stop signal due to broadcaster error")]
    Broadcast,
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("Reached max holepunch attempts - giving up")]
    MaxAttempts,
    #[error("Hole punching timed out")]
    Timeout,
    #[error("Broadcast receive: {0}")]
    BroadCastRecv(#[from] tokio::sync::broadcast::error::RecvError),
    #[error("Panic during task which sends outgoing packets")]
    Join(#[from] tokio::task::JoinError),
    #[error("Could not send holepunch packet - channel closed")]
    ChannelClosed,
    #[error("Hole punch send task closed before reporting send result for {0}")]
    SendTaskClosed(SocketAddr),
    #[error("No hole punch socket was returned")]
    SocketChannelClosed,
}

#[cfg(test)]
mod tests {
    use super::random_nonzero_port;
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn random_nonzero_port_never_returns_zero() {
        let mut rng = StdRng::seed_from_u64(1);

        for _ in 0..10_000 {
            assert_ne!(random_nonzero_port(&mut rng), 0);
        }
    }
}
