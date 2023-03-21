use self::{hole_punch::PunchingUdpSocket, mdns::mdns_server, mqtt::mqtt_client, topic::Topic};
use anyhow::anyhow;
use local_ip_address::local_ip;
use log::debug;
use quinn::AsyncUdpSocket;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use stunclient::StunClient;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

pub mod capability;
pub mod hole_punch;
pub mod mdns;
pub mod mqtt;
pub mod topic;

pub const TOKEN_LENGTH: usize = 32;
pub type SessionToken = [u8; 32];

pub struct DiscoveredPeer {
    pub addr: SocketAddr,
    pub token: SessionToken,
    pub topic: Option<Topic>,
    // pub discovery_method,
}

// TODO allow dynamically adding / removing topics
/// Setup peer discovery
pub async fn discover_peers(
    topics: Vec<Topic>,
    use_mdns: bool,
    use_mqtt: bool,
) -> anyhow::Result<(
    PunchingUdpSocket,
    UnboundedReceiver<DiscoveredPeer>,
    SessionToken,
)> {
    let (peers_tx, peers_rx) = unbounded_channel();

    let my_local_ip = local_ip()?;
    let raw_socket = tokio::net::UdpSocket::bind(SocketAddr::new(my_local_ip, 0)).await?;

    let (public_addr, nat_type) = stun_test(&raw_socket).await?;

    let (socket, hole_puncher) = PunchingUdpSocket::bind(raw_socket).await?;
    let addr = socket.local_addr()?;

    let id = &addr.to_string(); // TODO id should be derived from public key (probably)

    let mut rng = rand::thread_rng();
    let token: [u8; 32] = rng.gen();
    let single_topic = topics[0].clone();

    if use_mdns && is_private(my_local_ip) {
        mdns_server(id, addr, single_topic, peers_tx.clone(), token).await?;
    };
    if use_mqtt {
        mqtt_client(
            id.to_string(),
            topics,
            public_addr,
            nat_type,
            token,
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

/// Get our public address and NAT type using STUN
async fn stun_test(socket: &tokio::net::UdpSocket) -> anyhow::Result<(SocketAddr, NatType)> {
    // TODO have a list of public stun servers and choose two randomly
    let stun_client1 = StunClient::with_google_stun_server();
    let public_addr1 = stun_client1.query_external_address_async(socket).await?;

    let stun_server = "stun2.l.google.com:19302"
        .to_socket_addrs()?
        .find(|x| x.is_ipv4())
        .ok_or_else(|| anyhow!("Failed to get IP of stun server"))?;
    let stun_client2 = StunClient::new(stun_server);
    let public_addr2 = stun_client2.query_external_address_async(socket).await?;

    // TODO here we should loop over IPs of all network interfaces
    let addr = socket.local_addr()?;
    let has_nat = addr.ip() != public_addr1.ip();
    let is_symmetric = public_addr1 != public_addr2;

    let nat_type = if !has_nat {
        NatType::NoNat
    } else if is_symmetric {
        NatType::Symmetric
    } else {
        NatType::Asymmetric
    };

    debug!(
        "Local address: {:?}  Public address 1: {:?} Public address 2: {:?} NAT: {:?}",
        addr, public_addr1, public_addr2, nat_type
    );

    Ok((public_addr2, nat_type))
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub enum NatType {
    NoNat = 1,
    Asymmetric = 2,
    Symmetric = 3,
}

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
