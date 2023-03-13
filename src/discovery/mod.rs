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
use std::net::SocketAddr;
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
    let stun_client = StunClient::with_google_stun_server();
    let public_addr = stun_client
        .query_external_address_async(&raw_socket)
        .await?;
    let (socket, hole_puncher) = PunchingUdpSocket::bind(raw_socket).await?;
    let addr = socket.local_addr()?;

    debug!("Local address: {:?} Public address {:?}", addr, public_addr);

    let id = &addr.to_string(); // TODO id should be derived from public key (probably)

    let single_topic = topics[0].clone();
    let (capability, token) = handshake_request(&single_topic, addr);

    if use_mdns {
        mdns_server(id, addr, single_topic, peers_tx.clone(), capability).await?;
    };
    if use_mqtt {
        mqtt_client(id.to_string(), topics, public_addr, peers_tx, hole_puncher).await?;
    }
    Ok((socket, peers_rx, token))
}
