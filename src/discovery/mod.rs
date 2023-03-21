use self::{
    hole_punch::PunchingUdpSocket, mdns::mdns_server, mqtt::mqtt_client, stun::stun_test,
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
