//! Peer discovery
use self::{
    hole_punch::PunchingUdpSocket,
    mdns::MdnsServer,
    mqtt::MqttClient,
    stun::{stun_test, NatType},
    topic::Topic,
};
use local_ip_address::local_ip;
use log::debug;
use quinn::AsyncUdpSocket;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver},
    oneshot,
};

pub mod capability;
pub mod hole_punch;
pub mod mdns;
pub mod mqtt;
pub mod stun;
pub mod topic;

/// Length of a SessionToken
pub const TOKEN_LENGTH: usize = 32;
/// A session token used in capability verification (proof of knowledge of topic name)
pub type SessionToken = [u8; 32];

/// Database values for recording whether we are connected to a topic
const JOINED: [u8; 1] = [1];
const LEFT: [u8; 1] = [0];

/// Details of a peer found through one of the discovery methods
pub struct DiscoveredPeer {
    pub addr: SocketAddr,
    pub token: SessionToken,
    pub topic: Option<Topic>,
    // pub discovery_method,
}

/// Handles the different peer discovery methods
pub struct PeerDiscovery {
    pub peers_rx: UnboundedReceiver<DiscoveredPeer>,
    pub session_token: SessionToken,
    mdns_server: Option<MdnsServer>,
    mqtt_client: Option<MqttClient>,
    // waku_discovery: Option<WakuDiscovery>,
    pub topics_db: sled::Tree,
}

impl PeerDiscovery {
    pub async fn new(
        initial_topics: Vec<Topic>,
        // Whether to use mdns
        use_mdns: bool,
        // Wheter to use mqtt discovery
        use_mqtt: bool,
        public_key: [u8; 32],
        topics_db: sled::Tree,
    ) -> anyhow::Result<(PunchingUdpSocket, Self)> {
        // Join topics given as arguments, as well as from db
        let mut topics_to_join: HashSet<Topic> = get_topic_names(&topics_db)
            .iter()
            .filter_map(|(name, join)| {
                if *join {
                    Some(Topic::new(name.clone()))
                } else {
                    None
                }
            })
            .collect();

        for topic in initial_topics {
            topics_to_join.insert(topic);
        }

        // Channel for reporting discovered peers
        let (peers_tx, peers_rx) = unbounded_channel();

        let my_local_ip = local_ip()?;
        let raw_socket = tokio::net::UdpSocket::bind(SocketAddr::new(my_local_ip, 0)).await?;

        // Get our public address and NAT type from a STUN server
        // TODO make this offline-first by if we have an error and mqtt is disabled, ignore the
        // error
        let (public_ip, nat_type) = stun_test(&raw_socket).await?;

        let raw_socket_2 = tokio::net::UdpSocket::bind(SocketAddr::new(my_local_ip, 0)).await?;
        let (socket, hole_puncher) = PunchingUdpSocket::bind(raw_socket).await?;
        let (socket_2, hole_puncher_2) = PunchingUdpSocket::bind(raw_socket_2).await?;

        let addr = socket.local_addr()?;

        let public_addr = SocketAddr::new(public_ip, addr.port());

        // Id is used as an identifier for mdns services
        // TODO this should be hashed or rather use the session token for privacy
        let id = hex::encode(public_key);

        let mut rng = rand::thread_rng();
        let session_token: [u8; 32] = rng.gen();

        // Only use mdns if we are on a local network
        let mdns_server = if use_mdns && is_private(my_local_ip) {
            Some(
                MdnsServer::new(
                    &id,
                    addr,
                    peers_tx.clone(),
                    session_token,
                    topics_to_join.clone(),
                )
                .await?,
            )
        } else {
            None
        };

        let mqtt_client = if use_mqtt {
            Some(
                MqttClient::new(
                    id,
                    AnnounceAddress {
                        public_addr,
                        nat_type,
                        token: session_token,
                    },
                    peers_tx,
                    hole_puncher,
                )
                .await?,
            )
        } else {
            None
        };
        // let waku_discovery = if use_waku {
        //     Some(
        //         WakuDiscovery::new(
        //             AnnounceAddress {
        //                 public_addr,
        //                 nat_type,
        //                 token: session_token,
        //             },
        //             peers_tx,
        //             hole_puncher,
        //         )
        //         .await?,
        //     )
        // } else {
        //     None
        // };

        let mut peer_discovery = Self {
            peers_rx,
            session_token,
            mdns_server,
            mqtt_client,
            // waku_discovery,
            topics_db,
        };

        for topic in topics_to_join {
            peer_discovery.join_topic(topic).await?;
        }

        Ok((socket_2, peer_discovery))
    }

    /// Join the given topic
    pub async fn join_topic(&mut self, topic: Topic) -> anyhow::Result<()> {
        if let Some(mdns_server) = &self.mdns_server {
            mdns_server.add_topic(topic.clone()).await?;
        }

        if let Some(mqtt_client) = &self.mqtt_client {
            mqtt_client.add_topic(topic.clone()).await?;
        }

        // if let Some(waku_discovery) = &self.waku_discovery {
        //     waku_discovery.add_topic(topic.clone()).await?;
        // }

        self.topics_db.insert(&topic.name, &JOINED)?;
        Ok(())
    }

    /// Leave the given topic
    pub async fn leave_topic(&mut self, topic: Topic) -> anyhow::Result<()> {
        if let Some(mdns_server) = &self.mdns_server {
            mdns_server.remove_topic(topic.clone()).await?;
        }

        // if let Some(waku_discovery) = &self.waku_discovery {
        //     waku_discovery.remove_topic(topic.clone()).await?;
        // }

        if let Some(mqtt_client) = &self.mqtt_client {
            mqtt_client.remove_topic(topic.clone()).await?;
        }

        self.topics_db.insert(&topic.name, &LEFT)?;
        Ok(())
    }

    /// Get topic names, and whether or not we are currently connected
    pub fn get_topic_names(&self) -> Vec<(String, bool)> {
        get_topic_names(&self.topics_db)
    }
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

/// A message passed when joining or leaving a Topic.
/// The oneshot is to indicate whether or not the topic
/// was successfully joined or left
#[derive(Debug)]
pub enum JoinOrLeaveEvent {
    Join(Topic, oneshot::Sender<bool>),
    Leave(Topic, oneshot::Sender<bool>),
}

/// The payload of the encrypted message used to announce ourselves to remote peers
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct AnnounceAddress {
    public_addr: SocketAddr,
    nat_type: NatType,
    token: SessionToken,
}

/// Decide whether to initiate a connection to a peer or wait for them
/// to connect to us, and whether to attempt to hole punch to them.
pub fn should_connect_to_peer(
    remote_peer_announce: &AnnounceAddress,
    announce_address: &AnnounceAddress,
) -> (bool, bool) {
    if remote_peer_announce == announce_address {
        debug!("Found our own announce message");
        return (false, false);
    }

    // Dont connect if we are both on the same IP - use mdns
    if remote_peer_announce.public_addr.ip() == announce_address.public_addr.ip() {
        debug!("Found remote peer with the same public ip as ours - ignoring");
        return (false, false);
    }

    // TODO there are more cases when we should not bother hole punching
    let should_hole_punch = remote_peer_announce.nat_type != NatType::Symmetric;

    // Decide whether to initiate the connection deterministically
    // so that only one party initiates
    let our_nat_badness = announce_address.nat_type as u8;
    let their_nat_badness = remote_peer_announce.nat_type as u8;
    let should_initiate_connection = if our_nat_badness == their_nat_badness {
        // If we both have the same NAT type, use the socket address
        // as a tie breaker
        let us = announce_address.public_addr.to_string();
        let them = remote_peer_announce.public_addr.to_string();
        us > them
    } else {
        // Otherwise the peer with the worst NAT type initiates the
        // connection
        our_nat_badness > their_nat_badness
    };

    (should_initiate_connection, should_hole_punch)
}

fn get_topic_names(topics_db: &sled::Tree) -> Vec<(String, bool)> {
    topics_db
        .iter()
        .filter_map(|kv_result| {
            if let Ok((topic_name_buf, joined_buf)) = kv_result {
                // join or leave
                if let Ok(topic_name) = std::str::from_utf8(&topic_name_buf.to_vec()) {
                    match joined_buf.to_vec().get(0) {
                        Some(1) => Some((topic_name.to_string(), true)),
                        Some(0) => Some((topic_name.to_string(), false)),
                        _ => None,
                    }
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect()
}
