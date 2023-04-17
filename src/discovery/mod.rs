use self::{
    hole_punch::PunchingUdpSocket, mdns::mdns_server, mqtt::MqttClient, stun::stun_test,
    topic::Topic,
};
use local_ip_address::local_ip;
use quinn::AsyncUdpSocket;
use rand::Rng;
use std::net::{IpAddr, SocketAddr};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

pub mod capability;
pub mod hole_punch;
pub mod mdns;
pub mod mqtt;
pub mod stun;
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
// TODO allow separate lists of announce topics and lookup topic
/// Setup peer discovery
pub async fn discover_peers(
    topics: Vec<Topic>,
    // Whether to use mdns
    use_mdns: bool,
    // Whether to use mqtt
    use_mqtt: bool,
    public_key: [u8; 32],
) -> anyhow::Result<(
    PunchingUdpSocket,
    UnboundedReceiver<DiscoveredPeer>,
    SessionToken,
)> {
    let (peers_tx, peers_rx) = unbounded_channel();

    let my_local_ip = local_ip()?;
    let raw_socket = tokio::net::UdpSocket::bind(SocketAddr::new(my_local_ip, 0)).await?;

    // Get our public address and NAT type from a STUN server
    let (public_addr, nat_type) = stun_test(&raw_socket).await?;

    let (socket, hole_puncher) = PunchingUdpSocket::bind(raw_socket).await?;
    let addr = socket.local_addr()?;

    // TODO this should maybe be hashed
    let id = hex::encode(public_key);

    let mut rng = rand::thread_rng();
    let token: [u8; 32] = rng.gen();

    if use_mdns && is_private(my_local_ip) {
        mdns_server(&id, addr, topics.clone(), peers_tx.clone(), token).await?;
    };

    if use_mqtt {
        MqttClient::new(
            id,
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
