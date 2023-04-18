use self::{
    hole_punch::PunchingUdpSocket, mdns::MdnsServer, mqtt::MqttClient, stun::stun_test,
    topic::Topic,
};
use local_ip_address::local_ip;
use quinn::AsyncUdpSocket;
use rand::Rng;
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
};
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

pub struct PeerDiscovery {
    pub peers_rx: UnboundedReceiver<DiscoveredPeer>,
    pub session_token: SessionToken,
    mdns_server: Option<MdnsServer>,
    mqtt_client: Option<MqttClient>,
    pub connected_topics: HashSet<Topic>,
}

impl PeerDiscovery {
    pub async fn new(
        initial_topics: Vec<Topic>,
        // Whether to use mdns
        use_mdns: bool,
        // Whether to use mqtt
        use_mqtt: bool,
        public_key: [u8; 32],
    ) -> anyhow::Result<(PunchingUdpSocket, Self)> {
        let (peers_tx, peers_rx) = unbounded_channel();

        let my_local_ip = local_ip()?;
        let raw_socket = tokio::net::UdpSocket::bind(SocketAddr::new(my_local_ip, 0)).await?;

        // Get our public address and NAT type from a STUN server
        let (public_addr, nat_type) = stun_test(&raw_socket).await?;

        let (socket, hole_puncher) = PunchingUdpSocket::bind(raw_socket).await?;
        let addr = socket.local_addr()?;

        // Id is used as an identifier for the mqtt server, and mdns services
        // TODO this should be hashed or rather use the session token for privacy
        let id = hex::encode(public_key);

        let mut rng = rand::thread_rng();
        let session_token: [u8; 32] = rng.gen();

        let mdns_server = if use_mdns && is_private(my_local_ip) {
            Some(MdnsServer::new(&id, addr, peers_tx.clone(), session_token).await?)
        } else {
            None
        };

        let mqtt_client = if use_mqtt {
            Some(
                MqttClient::new(
                    id,
                    public_addr,
                    nat_type,
                    session_token,
                    peers_tx,
                    hole_puncher,
                )
                .await?,
            )
        } else {
            None
        };

        let mut peer_discovery = Self {
            peers_rx,
            session_token,
            mdns_server,
            mqtt_client,
            connected_topics: Default::default(),
        };

        for topic in initial_topics {
            peer_discovery.join_topic(topic).await?;
        }

        Ok((socket, peer_discovery))
    }

    pub async fn join_topic(&mut self, topic: Topic) -> anyhow::Result<()> {
        if let Some(mdns_server) = &self.mdns_server {
            mdns_server.add_topic(topic.clone()).await?;
        }

        if let Some(mqtt_client) = &self.mqtt_client {
            mqtt_client.add_topic(topic.clone()).await?;
        }

        self.connected_topics.insert(topic);
        Ok(())
    }

    pub async fn leave_topic(&mut self, topic: Topic) -> anyhow::Result<()> {
        if let Some(mdns_server) = &self.mdns_server {
            mdns_server.remove_topic(topic.clone()).await?;
        }

        if let Some(mqtt_client) = &self.mqtt_client {
            mqtt_client.remove_topic(topic.clone()).await?;
        }

        self.connected_topics.remove(&topic);
        Ok(())
    }
}

// TODO allow dynamically adding / removing topics
// TODO allow separate lists of announce topics and lookup topic
/// Setup peer discovery
// pub async fn discover_peers(
//     topics: Vec<Topic>,
//     // Whether to use mdns
//     use_mdns: bool,
//     // Whether to use mqtt
//     use_mqtt: bool,
//     public_key: [u8; 32],
// ) -> anyhow::Result<(
//     PunchingUdpSocket,
//     UnboundedReceiver<DiscoveredPeer>,
//     SessionToken,
// )> {
//     let (peers_tx, peers_rx) = unbounded_channel();
//
//     let my_local_ip = local_ip()?;
//     let raw_socket = tokio::net::UdpSocket::bind(SocketAddr::new(my_local_ip, 0)).await?;
//
//     // Get our public address and NAT type from a STUN server
//     let (public_addr, nat_type) = stun_test(&raw_socket).await?;
//
//     let (socket, hole_puncher) = PunchingUdpSocket::bind(raw_socket).await?;
//     let addr = socket.local_addr()?;
//
//     // TODO this should maybe be hashed
//     let id = hex::encode(public_key);
//
//     let mut rng = rand::thread_rng();
//     let token: [u8; 32] = rng.gen();
//
//     if use_mdns && is_private(my_local_ip) {
//         MdnsServer::new(&id, addr, topics.clone(), peers_tx.clone(), token).await?;
//     };
//
//     if use_mqtt {
//         MqttClient::new(
//             id,
//             topics,
//             public_addr,
//             nat_type,
//             token,
//             peers_tx,
//             hole_puncher,
//         )
//         .await?;
//     };
//
//     Ok((socket, peers_rx, token))
// }

// Check if an IP appears to be private
fn is_private(ip: IpAddr) -> bool {
    if let IpAddr::V4(ip_v4_addr) = ip {
        ip_v4_addr.is_private()
    } else {
        // In the case of ipv6 we cant be sure
        false
    }
}
