use anyhow::anyhow;
use futures::ready;
use log::{debug, info, warn};
use std::{
    io,
    net::SocketAddr,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::Interest,
    sync::{broadcast, mpsc},
};

/// Most of this is copied from https://github.com/Frando/quinn-holepunch

pub type UdpReceive = broadcast::Receiver<IncomingHolepunchPacket>;
pub type UdpSend = mpsc::Sender<OutgoingHolepunchPacket>;

const MAX_HOLEPUNCH_ATTEMPTS: usize = 10;

#[derive(Debug)]
pub struct PunchingUdpSocket {
    socket: Arc<tokio::net::UdpSocket>,
    quinn_socket_state: quinn_udp::UdpSocketState,
    udp_recv_tx: broadcast::Sender<IncomingHolepunchPacket>,
}

impl PunchingUdpSocket {
    pub async fn bind(socket: tokio::net::UdpSocket) -> io::Result<(Self, HolePuncher)> {
        let socket = socket.into_std()?;

        quinn_udp::UdpSocketState::configure((&socket).into())?;

        let socket = Arc::new(tokio::net::UdpSocket::from_std(socket)?);

        let (udp_recv_tx, _udp_recv) = broadcast::channel::<IncomingHolepunchPacket>(1024);
        let (udp_send, mut udp_send_rx) = mpsc::channel::<OutgoingHolepunchPacket>(1024);

        let socket_clone = socket.clone();
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
                quinn_socket_state: quinn_udp::UdpSocketState::new(),
                udp_recv_tx: udp_recv_tx.clone(),
            },
            HolePuncher {
                udp_send,
                udp_recv_tx,
            },
        ))
    }
}

impl quinn::AsyncUdpSocket for PunchingUdpSocket {
    fn poll_send(
        &mut self,
        state: &quinn_udp::UdpState,
        cx: &mut Context,
        transmits: &[quinn::Transmit],
    ) -> Poll<io::Result<usize>> {
        let quinn_socket_state = &mut self.quinn_socket_state;
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
            let _ = channel.send(packet);
        }
    }
}

#[derive(Clone, Debug)]
pub struct IncomingHolepunchPacket {
    data: [u8; 1],
    from: SocketAddr,
}

#[derive(Clone, Debug)]
pub struct OutgoingHolepunchPacket {
    data: [u8; 1],
    dest: SocketAddr,
}

#[derive(Clone, Debug)]
pub struct HolePuncher {
    udp_send: UdpSend,
    udp_recv_tx: broadcast::Sender<IncomingHolepunchPacket>,
}

impl HolePuncher {
    /// Make a connection by holepunching
    /// TODO dont wait forever - give up after n tries
    pub async fn hole_punch_peer(&mut self, addr: SocketAddr) -> anyhow::Result<()> {
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
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            tokio::select! {
              send = self.udp_send.send(packet.clone()) => {
                  if let Err(err) = send {
                      warn!("Failed to forward holepunch packet to {addr}: {err}");
                  } else if packet.data == [0u8] {
                      debug!("sent initial packet to {addr}, waiting");
                      attempts += 1;
                      if attempts >= MAX_HOLEPUNCH_ATTEMPTS {
                          return Err(anyhow!("Reached max holepunch attempts - giving up"));
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
                      }
                  }
                  wait = false
              }
            }
        }
        Ok(())
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
