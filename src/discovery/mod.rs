use self::{
    handshake::{handshake_request, Token},
    hole_punch::PunchingUdpSocket,
    mdns::mdns_server,
    mqtt::mqtt_client,
    topic::Topic,
};
use local_ip_address::local_ip;
use log::debug;
use quinn::AsyncUdpSocket;
use std::net::{IpAddr, SocketAddr};
use stunclient::StunClient;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

pub mod handshake;
pub mod hole_punch;
pub mod mdns;
pub mod mqtt;
pub mod topic;

pub struct DiscoveredPeer {
    pub addr: SocketAddr,
    pub token: Option<Token>,
    pub topic: Option<Topic>,
    // pub discovery_method,
}

// TODO allow dynamically adding / removing topics

pub async fn discover_peers(
    topics: Vec<Topic>,
    use_mdns: bool,
    use_mqtt: bool,
) -> anyhow::Result<(PunchingUdpSocket, UnboundedReceiver<DiscoveredPeer>, Token)> {
    let (peers_tx, peers_rx) = unbounded_channel();

    let my_local_ip = local_ip()?;
    let raw_socket = tokio::net::UdpSocket::bind(SocketAddr::new(my_local_ip, 0)).await?;

    // Get our public address with STUN
    let stun_client = StunClient::with_google_stun_server();
    let public_addr = stun_client
        .query_external_address_async(&raw_socket)
        .await?;

    let (socket, hole_puncher) = PunchingUdpSocket::bind(raw_socket).await?;
    let addr = socket.local_addr()?;

    // TODO here we should loop over IPs of all network interfaces
    let has_nat = addr.ip() != public_addr.ip();

    debug!(
        "Local address: {:?} Public address {:?} NAT: {}",
        addr, public_addr, has_nat
    );

    let id = &addr.to_string(); // TODO id should be derived from public key (probably)

    let single_topic = topics[0].clone();
    let (capability, token) = handshake_request(&single_topic, addr);

    if use_mdns && is_private(my_local_ip) {
        mdns_server(id, addr, single_topic, peers_tx.clone(), capability).await?;
    };
    if use_mqtt {
        mqtt_client(
            id.to_string(),
            topics,
            public_addr,
            has_nat,
            peers_tx,
            hole_puncher,
        )
        .await?;
    };
    Ok((socket, peers_rx, token))
}

// Check if an IP appears to be private
fn is_private(ip: IpAddr) -> bool {
    if let IpAddr::V4(ip_v4_addr) = ip {
        ip_v4_addr.is_private()
    } else {
        // In the case of ipv6 we cant be sure
        false
    }
}

//
// fn addresses_identical(ip1: IpAddr, ip2: IpAddr) -> bool {
//     ip1 == ip2
// match ip1 {
//     IpAddr::V4(ip_v4_addr1) => {
//         if let IpAddr::V4(ip_v4_addr2) = ip2 {
//             ip_v4_addr1 == ip_v4_addr2
//         } else {
//             false
//         }
//     }
//     IpAddr::V6(ip_v6_addr1) => {
//         if let IpAddr::V6(ip_v6_addr2) = ip2 {
//             ip_v6_addr1 == ip_v6_addr2
//         } else {
//             false
//         }
//     }
// }
