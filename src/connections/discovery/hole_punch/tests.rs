use super::*;
use crate::connections::quic::generate_certificate;
use futures::future::poll_fn;
use quinn::{AsyncUdpSocket, ClientConfig, Connection, Endpoint, ServerConfig, TransportConfig};
use quinn_udp::{EcnCodepoint, RecvMeta, Transmit};
use std::{
    io::IoSliceMut,
    net::{Ipv4Addr, Ipv6Addr},
};
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn holepunch_inspection_respects_datagram_boundaries_and_preserves_input() {
    let (tx, mut rx) = broadcast::channel(16);
    let mut data = vec![
        vec![0, 77],            // One datagram, followed by unused buffer space.
        vec![1, 0, 1, 88],      // Three coalesced one-byte datagrams.
        vec![9; 1200 * 2 + 97], // QUIC-sized datagrams and a shorter final datagram.
        vec![8, 8, 1],          // A two-byte datagram followed by a one-byte datagram.
        vec![],                 // Empty receive, including a zero stride.
        vec![0],                // Outside msg_count; must not be inspected.
    ];
    let lengths = [1, 3, 2497, 3, 0, 1];
    let strides = [1, 1, 1200, 2, 0, 1];
    let metas: Vec<_> = lengths
        .into_iter()
        .zip(strides)
        .enumerate()
        .map(|(i, (len, stride))| RecvMeta {
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 1000 + i as u16)),
            len,
            stride,
            ecn: Some(EcnCodepoint::Ect0),
            dst_ip: Some(Ipv4Addr::LOCALHOST.into()),
        })
        .collect();
    let original_data = data.clone();
    let original_metas = format!("{metas:?}");
    let bufs: Vec<_> = data.iter_mut().map(|buf| IoSliceMut::new(buf)).collect();
    forward_holepunch(&tx, &bufs, &metas, 5);
    for (byte, port) in [(0, 1000), (1, 1001), (0, 1001), (1, 1001), (1, 1003)] {
        let packet = rx.try_recv().unwrap();
        assert_eq!(packet.data, [byte]);
        assert_eq!(packet.from, SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    }
    assert!(matches!(
        rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(data, original_data);
    assert_eq!(format!("{metas:?}"), original_metas);
}

async fn bind(ip: IpAddr) -> (Arc<PunchingUdpSocket>, HolePuncher) {
    let socket = UdpSocket::bind(SocketAddr::new(ip, 0)).await.unwrap();
    let (socket, puncher) = PunchingUdpSocket::bind(socket).await.unwrap();
    (Arc::new(socket), puncher)
}

async fn send(socket: &Arc<PunchingUdpSocket>, transmit: &Transmit<'_>) {
    let mut poller = socket.clone().create_io_poller();
    loop {
        match socket.try_send(transmit) {
            Ok(()) => return,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                poll_fn(|cx| poller.as_mut().poll_writable(cx))
                    .await
                    .unwrap();
            }
            Err(error) => panic!("UDP send failed: {error}"),
        }
    }
}

async fn receive(socket: &PunchingUdpSocket) -> Vec<(RecvMeta, Vec<u8>)> {
    // Match Quinn's allocation rule so GRO cannot truncate a coalesced receive.
    let mut data = vec![vec![0; 1472 * socket.max_receive_segments()]; 4];
    let mut bufs: Vec<_> = data.iter_mut().map(|buf| IoSliceMut::new(buf)).collect();
    let mut metas = [RecvMeta::default(); 4];
    let count = poll_fn(|cx| socket.poll_recv(cx, &mut bufs, &mut metas))
        .await
        .unwrap();
    metas
        .iter()
        .zip(&bufs)
        .take(count)
        .flat_map(|(meta, buf)| {
            buf[..meta.len]
                .chunks(meta.stride.max(1))
                .map(|datagram| (*meta, datagram.to_vec()))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test]
async fn udp_payload_metadata_and_reusable_pollers() {
    timeout(DEADLINE, async {
        let (sender, puncher) = bind(Ipv4Addr::LOCALHOST.into()).await;
        let (receiver, receiver_puncher) = bind(Ipv4Addr::LOCALHOST.into()).await;
        let mut packets = receiver_puncher.udp_recv_tx.subscribe();
        assert_eq!(
            sender.max_transmit_segments(),
            sender.quinn_socket_state.max_gso_segments()
        );
        assert_eq!(
            receiver.max_receive_segments(),
            receiver.quinn_socket_state.gro_segments()
        );
        assert_eq!(
            sender.may_fragment(),
            sender.quinn_socket_state.may_fragment()
        );

        let mut first = sender.clone().create_io_poller();
        let mut second = sender.clone().create_io_poller();
        for _ in 0..3 {
            let (a, b) = tokio::join!(
                poll_fn(|cx| first.as_mut().poll_writable(cx)),
                poll_fn(|cx| second.as_mut().poll_writable(cx)),
            );
            a.unwrap();
            b.unwrap();
        }
        let destination = receiver.local_addr().unwrap();
        send(
            &sender,
            &Transmit {
                destination,
                ecn: Some(EcnCodepoint::Ect0),
                contents: b"ordinary UDP payload",
                segment_size: None,
                src_ip: Some(sender.local_addr().unwrap().ip()),
            },
        )
        .await;
        let datagrams = receive(&receiver).await;
        assert_eq!(datagrams.len(), 1);
        let (meta, payload) = &datagrams[0];
        assert_eq!(payload, b"ordinary UDP payload");
        assert_eq!(meta.addr, sender.local_addr().unwrap());
        if let Some(ip) = meta.dst_ip {
            assert_eq!(ip, destination.ip());
        }
        if let Some(ecn) = meta.ecn {
            assert_eq!(ecn, EcnCodepoint::Ect0);
        }
        assert!(
            packets.try_recv().is_err(),
            "ordinary data is not a hole punch"
        );

        send_holepunch_packet(
            &puncher.udp_send,
            OutgoingHolepunchPacket::new_init(destination),
        )
        .await
        .unwrap();
        let received = receive(&receiver).await;
        assert_eq!(received[0].1, INIT_PUNCH);
        let packet = packets.recv().await.unwrap();
        assert_eq!(packet.data, INIT_PUNCH);
        assert_eq!(packet.from, sender.local_addr().unwrap());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn segmented_udp_preserves_each_payload_including_short_final_segment() {
    timeout(DEADLINE, async {
        let (sender, _puncher) = bind(Ipv4Addr::LOCALHOST.into()).await;
        let (receiver, _receiver_puncher) = bind(Ipv4Addr::LOCALHOST.into()).await;
        let segments = sender.max_transmit_segments().min(4);
        if segments < 2 {
            eprintln!("Skipping segmented UDP test: GSO is unavailable");
            return;
        }
        let mut expected: Vec<_> = (0..segments - 1).map(|i| vec![i as u8; 1200]).collect();
        expected.push(vec![99; 23]);
        let contents = expected.concat();
        send(
            &sender,
            &Transmit {
                destination: receiver.local_addr().unwrap(),
                ecn: None,
                contents: &contents,
                segment_size: Some(1200),
                src_ip: None,
            },
        )
        .await;
        if sender.max_transmit_segments() == 1 {
            eprintln!("Skipping segmented UDP test: GSO was disabled after a send error");
            return;
        }
        let mut received = Vec::new();
        while received.len() < expected.len() {
            for (meta, payload) in receive(&receiver).await {
                assert_eq!(meta.addr, sender.local_addr().unwrap());
                received.push(payload);
            }
        }
        // The OS may deliver separate datagrams or coalesce them. Either must work.
        assert_eq!(received, expected);
    })
    .await
    .unwrap();
}

struct QuicPair {
    _client_endpoint: Endpoint,
    _server_endpoint: Endpoint,
    client_socket: Arc<PunchingUdpSocket>,
    server_socket: Arc<PunchingUdpSocket>,
    client: Connection,
    server: Connection,
    client_puncher: HolePuncher,
    server_puncher: HolePuncher,
}

impl QuicPair {
    async fn new(ip: IpAddr, offload: bool) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (client_socket, client_puncher) = bind(ip).await;
        let (server_socket, server_puncher) = bind(ip).await;
        let (cert, key) = generate_certificate().unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.clone()).unwrap();
        let mut client_config = ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();
        let mut transport = TransportConfig::default();
        transport.enable_segmentation_offload(offload);
        let transport = Arc::new(transport);
        client_config.transport_config(transport.clone());
        let mut server_config = ServerConfig::with_single_cert(vec![cert], key).unwrap();
        server_config.transport_config(transport);
        let server_endpoint = Endpoint::new_with_abstract_socket(
            Default::default(),
            Some(server_config),
            server_socket.clone(),
            Arc::new(quinn::TokioRuntime),
        )
        .unwrap();
        let mut client_endpoint = Endpoint::new_with_abstract_socket(
            Default::default(),
            None,
            client_socket.clone(),
            Arc::new(quinn::TokioRuntime),
        )
        .unwrap();
        client_endpoint.set_default_client_config(client_config);
        let connecting = client_endpoint
            .connect(server_endpoint.local_addr().unwrap(), "peer")
            .unwrap();
        let (client, server) = tokio::join!(connecting, async {
            server_endpoint.accept().await.unwrap().await.unwrap()
        });
        Self {
            _client_endpoint: client_endpoint,
            _server_endpoint: server_endpoint,
            client_socket,
            server_socket,
            client: client.unwrap(),
            server,
            client_puncher,
            server_puncher,
        }
    }

    async fn transfer(&self) {
        let payload: Vec<_> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let client = async {
            let (mut send, mut recv) = self.client.open_bi().await.unwrap();
            send.write_all(&payload).await.unwrap();
            send.finish().unwrap();
            assert_eq!(recv.read_to_end(payload.len()).await.unwrap(), payload);
        };
        let server = async {
            let (mut send, mut recv) = self.server.accept_bi().await.unwrap();
            let received = recv.read_to_end(payload.len()).await.unwrap();
            assert_eq!(received, payload);
            send.write_all(&received).await.unwrap();
            send.finish().unwrap();
        };
        tokio::join!(client, server);
    }

    async fn punch(&self) {
        let mut client_packets = self.client_puncher.udp_recv_tx.subscribe();
        let mut server_packets = self.server_puncher.udp_recv_tx.subscribe();
        let client_address = self.client_socket.local_addr().unwrap();
        let server_address = self.server_socket.local_addr().unwrap();
        for _ in 0..8 {
            send_holepunch_packet(
                &self.client_puncher.udp_send,
                OutgoingHolepunchPacket::new_init(server_address),
            )
            .await
            .unwrap();
            let init = server_packets.recv().await.unwrap();
            assert_eq!(init.data, INIT_PUNCH);
            assert_eq!(init.from, client_address);
            send_holepunch_packet(
                &self.server_puncher.udp_send,
                OutgoingHolepunchPacket::new_ack(client_address),
            )
            .await
            .unwrap();
            let ack = client_packets.recv().await.unwrap();
            assert_eq!(ack.data, ACK_PUNCH);
            assert_eq!(ack.from, server_address);
        }
    }

    async fn check_mtu(&self) {
        for (connection, socket) in [
            (&self.client, &self.client_socket),
            (&self.server, &self.server_socket),
        ] {
            if socket.may_fragment() {
                assert_eq!(connection.stats().path.sent_plpmtud_probes, 0);
                continue;
            }
            timeout(Duration::from_secs(3), async {
                loop {
                    let path = connection.stats().path;
                    if path.sent_plpmtud_probes > 0 && path.current_mtu > 1200 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("MTU discovery must probe beyond the initial 1200 bytes");
        }
    }
}

async fn quic_transfer_with_holepunch(ip: IpAddr, offload: bool) {
    timeout(DEADLINE, async {
        let pair = QuicPair::new(ip, offload).await;
        tokio::join!(pair.transfer(), pair.punch());
        pair.check_mtu().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn quic_ipv4_bulk_transfer_holepunch_and_mtu_discovery() {
    quic_transfer_with_holepunch(Ipv4Addr::LOCALHOST.into(), true).await;
}

#[tokio::test]
async fn quic_bulk_transfer_without_segmentation_offload() {
    quic_transfer_with_holepunch(Ipv4Addr::LOCALHOST.into(), false).await;
}

#[tokio::test]
async fn quic_ipv6_bulk_transfer_holepunch_and_mtu_discovery() {
    match UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await {
        Ok(socket) => drop(socket),
        Err(error) => {
            eprintln!("Skipping IPv6 test: loopback IPv6 unavailable ({error})");
            return;
        }
    }
    quic_transfer_with_holepunch(Ipv6Addr::LOCALHOST.into(), true).await;
}
