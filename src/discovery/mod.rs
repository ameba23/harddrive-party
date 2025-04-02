//! Peer discovery
use self::{
    hole_punch::{birthday_hard_side, PunchingUdpSocket},
    mdns::MdnsServer,
    mqtt::MqttClient,
    stun::stun_test,
    topic::Topic,
};
use anyhow::anyhow;
use bincode::{deserialize, serialize};
use harddrive_party_shared::ui_messages::UiTopic;
use hole_punch::HolePuncher;
use local_ip_address::local_ip;
use log::{debug, error};
use quinn::AsyncUdpSocket;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
};
use tokio::{
    net::UdpSocket,
    sync::{
        mpsc::{channel, Receiver, Sender},
        oneshot,
    },
};
use topic::TopicsDb;

pub mod capability;
pub mod hole_punch;
pub mod mdns;
pub mod mqtt;
pub mod stun;
pub mod topic;

/// Length of a SessionToken
pub const TOKEN_LENGTH: usize = 32;
/// A session token used in capability verification (proof of knowledge of topic name)
pub type SessionToken = [u8; TOKEN_LENGTH];

#[repr(u8)]
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum PeerConnectionDetails {
    NoNat(SocketAddr) = 1,
    Asymmetric(SocketAddr) = 2,
    Symmetric(IpAddr) = 3,
}

impl PeerConnectionDetails {
    /// Gets the IP address
    pub fn ip(&self) -> IpAddr {
        match self {
            PeerConnectionDetails::NoNat(addr) => addr.ip(),
            PeerConnectionDetails::Asymmetric(addr) => addr.ip(),
            PeerConnectionDetails::Symmetric(ip) => *ip,
        }
    }
}

/// Details of a peer found through one of the discovery methods
#[derive(Debug)]
pub struct DiscoveredPeer {
    pub socket_address: SocketAddr,
    pub socket_option: Option<UdpSocket>,
    pub token: SessionToken,
    pub topic: Option<Topic>,
    // pub discovery_method,
}

/// Handles the different peer discovery methods
pub struct PeerDiscovery {
    peers_tx: Sender<DiscoveredPeer>,
    pub peers_rx: Receiver<DiscoveredPeer>,
    pub session_token: SessionToken,
    mdns_server: Option<MdnsServer>,
    mqtt_client: Option<MqttClient>,
    pub topics_db: TopicsDb,
    hole_puncher: Option<HolePuncher>,
    announce_address: AnnounceAddress,
}

impl PeerDiscovery {
    pub async fn new(
        initial_topics: Vec<Topic>,
        // Whether to use mDNS, MQTT or both
        discovery_methods: DiscoveryMethods,
        public_key: [u8; 32],
        topics_db: sled::Tree,
        mqtt_server: Option<String>,
    ) -> anyhow::Result<(Option<PunchingUdpSocket>, Self)> {
        let topics_db = TopicsDb::new(topics_db);
        // Join topics given as arguments, as well as from db
        let mut topics_to_join: HashSet<Topic> = topics_db
            .get_topics()
            .iter()
            .filter_map(|ui_topic| {
                if ui_topic.connected {
                    Some(Topic::new(ui_topic.name.clone()))
                } else {
                    None
                }
            })
            .collect();

        for topic in initial_topics {
            topics_to_join.insert(topic);
        }

        // Channel for reporting discovered peers
        let (peers_tx, peers_rx) = channel(1024);

        let my_local_ip = local_ip()?;
        let raw_socket = UdpSocket::bind(SocketAddr::new(my_local_ip, 0)).await?;

        // Get our public address and NAT type from a STUN server
        // TODO make this offline-first by if we have an error and mqtt is disabled, ignore the
        // error
        let local_connection_details = stun_test(&raw_socket).await?;

        let (socket, hole_puncher) = PunchingUdpSocket::bind(raw_socket).await?;

        // Only use the hole_puncher if we are not behind symmetric nat
        let hole_puncher = match local_connection_details {
            PeerConnectionDetails::Symmetric(_) => None,
            _ => Some(hole_puncher.clone()),
        };

        let addr = socket.local_addr()?;

        // Id is used as an identifier for mdns services
        // TODO this should be hashed or rather use the session token for privacy
        let id = hex::encode(public_key);

        let mut rng = rand::thread_rng();
        let session_token: [u8; 32] = rng.gen();

        // Only use mdns if we are on a local network
        let mdns_server = if discovery_methods.use_mdns() && is_private(my_local_ip) {
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

        let socket_option = match local_connection_details {
            // TODO probable need to give the ip address here
            PeerConnectionDetails::Symmetric(_) => None,
            _ => Some(socket),
        };

        let announce_address = AnnounceAddress {
            connection_details: local_connection_details.clone(),
            token: session_token,
        };

        let mqtt_client = if discovery_methods.use_mqtt() {
            Some(
                MqttClient::new(
                    announce_address.clone(),
                    peers_tx.clone(),
                    hole_puncher.clone(),
                    mqtt_server,
                )
                .await?,
            )
        } else {
            None
        };

        let mut peer_discovery = Self {
            peers_tx,
            peers_rx,
            session_token,
            mdns_server,
            mqtt_client,
            topics_db,
            hole_puncher,
            announce_address,
        };

        for topic in topics_to_join {
            peer_discovery.join_topic(topic).await?;
        }

        Ok((socket_option, peer_discovery))
    }

    /// Join the given topic
    pub async fn join_topic(&mut self, topic: Topic) -> anyhow::Result<()> {
        if let Some(mdns_server) = &self.mdns_server {
            mdns_server.add_topic(topic.clone()).await?;
        }

        if let Some(mqtt_client) = &self.mqtt_client {
            mqtt_client.add_topic(topic.clone()).await?;
        }

        //TODO encrypt self.announce_address with this topic
        let announce_address = serialize(&self.announce_address)?;
        let announce_payload = topic.encrypt(&announce_address);

        self.topics_db.join(&topic, announce_payload)?;
        Ok(())
    }

    /// Leave the given topic
    pub async fn leave_topic(&mut self, topic: Topic) -> anyhow::Result<()> {
        if let Some(mdns_server) = &self.mdns_server {
            mdns_server.remove_topic(topic.clone()).await?;
        }

        if let Some(mqtt_client) = &self.mqtt_client {
            mqtt_client.remove_topic(topic.clone()).await?;
        }

        self.topics_db.leave(&topic)?;
        Ok(())
    }

    /// Get topic names, and whether or not we are currently connected
    pub fn get_topic_names(&self) -> Vec<UiTopic> {
        self.topics_db.get_topics()
    }

    pub async fn connect_direct_to_peer(&self, announce_payload: &[u8]) -> anyhow::Result<()> {
        let topics: Vec<Topic> = self
            .get_topic_names()
            .into_iter()
            // This will get all known topics, even if we are not currently joined
            .map(|ui_topic| Topic::new(ui_topic.name))
            .collect();
        for topic in topics.iter() {
            if let Some(announce_address) = decrypt_using_topic(announce_payload, topic) {
                // TODO check it is not ourself
                debug!("Remote peer {:?}", announce_address);
                let connection_details = announce_address.connection_details.clone();
                return match handle_peer(
                    self.hole_puncher.clone(),
                    connection_details,
                    announce_address,
                )
                .await
                {
                    Ok(Some(discovered_peer)) => {
                        debug!("Connect to {:?}", discovered_peer);
                        if self.peers_tx.send(discovered_peer).await.is_err() {
                            error!("Cannot write to channel");
                        }
                        Ok(())
                    }
                    Ok(None) => {
                        debug!("Successfully handled peer - awaiting connection from their side");
                        Ok(())
                    }
                    Err(error) => Err(anyhow!("Error when handling discovered peer {:?}", error)),
                };
            }
        }
        Err(anyhow!("Could not decrypt accounce message"))
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
    connection_details: PeerConnectionDetails,
    token: SessionToken,
}

pub async fn handle_peer(
    hole_puncher: Option<HolePuncher>,
    local: PeerConnectionDetails,
    remote: AnnounceAddress,
) -> anyhow::Result<Option<DiscoveredPeer>> {
    match remote.connection_details {
        PeerConnectionDetails::Symmetric(remote_ip) => match local {
            PeerConnectionDetails::Symmetric(_) => {
                Err(anyhow!("Symmetric to Symmetric not yet supported"))
            }
            PeerConnectionDetails::Asymmetric(_) => match hole_puncher {
                Some(mut puncher) => {
                    let _socket_address = puncher.hole_punch_peer_without_port(remote_ip).await?;
                    // Wait for them to connect to us
                    Ok(None)
                }
                None => Err(anyhow!("We have asymmetric nat but no local socket")),
            },
            PeerConnectionDetails::NoNat(_) => {
                // They are symmetric (hard), we have no nat
                // Wait for them to connect
                Ok(None)
            }
        },
        PeerConnectionDetails::Asymmetric(socket_address) => {
            match local {
                PeerConnectionDetails::Asymmetric(our_socket_address) => match hole_puncher {
                    Some(mut puncher) => {
                        puncher.hole_punch_peer(socket_address).await?;
                        // Decide whether to connect or let them connect, by lexicographically
                        // comparing socket addresses
                        Ok(if our_socket_address > socket_address {
                            Some(DiscoveredPeer {
                                socket_address,
                                socket_option: None,
                                token: remote.token,
                                topic: None,
                            })
                        } else {
                            None
                        })
                    }
                    None => Err(anyhow!("We have asymmetric nat but no local socket")),
                },
                PeerConnectionDetails::Symmetric(_) => {
                    let (socket, socket_address) = birthday_hard_side(socket_address).await?;
                    Ok(Some(DiscoveredPeer {
                        socket_address,
                        socket_option: Some(socket),
                        token: remote.token,
                        topic: None,
                    }))
                }
                PeerConnectionDetails::NoNat(_) => {
                    // they are Asymmetric (easy), we have no nat
                    // just wait for them to connect
                    Ok(None)
                }
            }
        }
        PeerConnectionDetails::NoNat(socket_address) => {
            // They have no nat - should be able to connect to them normally
            match local {
                PeerConnectionDetails::NoNat(our_socket_address) => {
                    // Need to decide whether to connect
                    Ok(if our_socket_address > socket_address {
                        Some(DiscoveredPeer {
                            socket_address,
                            socket_option: None,
                            token: remote.token,
                            topic: None,
                        })
                    } else {
                        None
                    })
                }
                PeerConnectionDetails::Symmetric(_) => {
                    let socket = UdpSocket::bind("0.0.0.0:0").await?;
                    Ok(Some(DiscoveredPeer {
                        socket_address,
                        socket_option: Some(socket),
                        token: remote.token,
                        topic: None,
                    }))
                }
                _ => Ok(Some(DiscoveredPeer {
                    socket_address,
                    socket_option: None,
                    token: remote.token,
                    topic: None,
                })),
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DiscoveryMethods {
    MqttOnly,
    MdnsOnly,
    MqttAndMdns,
}

impl DiscoveryMethods {
    fn use_mdns(&self) -> bool {
        self != &DiscoveryMethods::MqttOnly
    }

    fn use_mqtt(&self) -> bool {
        self != &DiscoveryMethods::MdnsOnly
    }
}

pub fn decrypt_using_topic(payload: &[u8], topic: &Topic) -> Option<AnnounceAddress> {
    if let Some(announce_message_bytes) = topic.decrypt(payload) {
        let announce_address_result: Result<AnnounceAddress, Box<bincode::ErrorKind>> =
            deserialize(&announce_message_bytes);
        if let Ok(announce_address) = announce_address_result {
            return Some(announce_address);
        }
    }
    None
}
