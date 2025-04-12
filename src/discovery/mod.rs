//! Peer discovery
use self::{
    hole_punch::{birthday_hard_side, PunchingUdpSocket},
    mdns::MdnsServer,
    stun::stun_test,
    topic::Topic,
};
use anyhow::anyhow;
use base64::prelude::{Engine as _, BASE64_STANDARD_NO_PAD};
use bincode::{deserialize, serialize};
use harddrive_party_shared::{ui_messages::UiTopic, wire_messages::PeerConnectionDetails};
use hole_punch::HolePuncher;
use local_ip_address::local_ip;
use log::{debug, error};
use quinn::AsyncUdpSocket;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
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
pub mod stun;
pub mod topic;

/// Length of a SessionToken
pub const TOKEN_LENGTH: usize = 32;
/// A session token used in capability verification (proof of knowledge of topic name)
pub type SessionToken = [u8; TOKEN_LENGTH];

/// Details of a peer found through one of the discovery methods
#[derive(Debug)]
pub struct DiscoveredPeer {
    pub socket_address: SocketAddr,
    pub socket_option: Option<UdpSocket>,
    pub public_key: [u8; 32],
    pub topic: Option<Topic>,
    // pub discovery_method,
}

/// Handles the different peer discovery methods
pub struct PeerDiscovery {
    peers_tx: Sender<DiscoveredPeer>,
    pub peers_rx: Receiver<DiscoveredPeer>,
    mdns_server: Option<MdnsServer>,
    pub topics_db: TopicsDb,
    hole_puncher: Option<HolePuncher>,
    announce_address: AnnounceAddress,
    pending_peer_connections: HashMap<SocketAddr, PeerConnectionDetails>,
}

impl PeerDiscovery {
    pub async fn new(
        initial_topics: Vec<Topic>,
        // Whether to use mDNS, MQTT or both
        discovery_methods: DiscoveryMethods,
        public_key: [u8; 32],
        topics_db: sled::Tree,
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

        // Only use mdns if we are on a local network
        let mdns_server = if discovery_methods.use_mdns() && is_private(my_local_ip) {
            Some(
                MdnsServer::new(
                    &id,
                    addr,
                    peers_tx.clone(),
                    topics_to_join.clone(),
                    public_key,
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
            public_key,
        };

        let mut peer_discovery = Self {
            peers_tx,
            peers_rx,
            mdns_server,
            topics_db,
            hole_puncher,
            announce_address,
            pending_peer_connections: Default::default(),
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

        self.topics_db.join(&topic)?;
        Ok(())
    }

    /// Leave the given topic
    pub async fn leave_topic(&mut self, topic: Topic) -> anyhow::Result<()> {
        if let Some(mdns_server) = &self.mdns_server {
            mdns_server.remove_topic(topic.clone()).await?;
        }

        self.topics_db.leave(&topic)?;
        Ok(())
    }

    /// Get topic names, and whether or not we are currently connected
    pub fn get_topic_names(&self) -> Vec<UiTopic> {
        self.topics_db.get_topics()
    }

    pub async fn connect_direct_to_peer(&mut self, announce_payload: &str) -> anyhow::Result<()> {
        let announce_address_bytes = BASE64_STANDARD_NO_PAD.decode(announce_payload)?;
        let announce_address: AnnounceAddress = deserialize(&announce_address_bytes)?;
        // TODO check it is not ourself
        debug!("Remote peer {:?}", announce_address);
        let connection_details = announce_address.connection_details.clone();
        return match handle_peer(
            self.hole_puncher.clone(),
            connection_details,
            announce_address.clone(),
        )
        .await
        {
            Ok((Some(discovered_peer), _)) => {
                debug!("Connect to {:?}", discovered_peer);
                if self.peers_tx.send(discovered_peer).await.is_err() {
                    error!("Cannot write to channel");
                }
                Ok(())
            }
            Ok((None, socket_address)) => {
                debug!("Successfully handled peer - awaiting connection from their side");
                // TODO here we need the full socket address to compare it with an incoming
                // connection
                self.add_pending_peer(socket_address, announce_address);
                Ok(())
            }
            Err(error) => Err(anyhow!("Error when handling discovered peer {:?}", error)),
        };
    }

    fn add_pending_peer(&mut self, socket_address: SocketAddr, announce_address: AnnounceAddress) {
        self.pending_peer_connections
            .insert(socket_address, announce_address.connection_details);
    }

    pub fn get_pending_peer(&self, socket_address: &SocketAddr) -> Option<&PeerConnectionDetails> {
        self.pending_peer_connections.get(socket_address)
    }

    pub fn get_ui_announce_address(&self) -> anyhow::Result<String> {
        let announce_address = serialize(&self.announce_address)?;
        Ok(BASE64_STANDARD_NO_PAD.encode(&announce_address))
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
    public_key: [u8; 32],
}

pub async fn handle_peer(
    hole_puncher: Option<HolePuncher>,
    local: PeerConnectionDetails,
    remote: AnnounceAddress,
) -> anyhow::Result<(Option<DiscoveredPeer>, SocketAddr)> {
    match remote.connection_details {
        PeerConnectionDetails::Symmetric(remote_ip) => match local {
            PeerConnectionDetails::Symmetric(_) => {
                Err(anyhow!("Symmetric to Symmetric not yet supported"))
            }
            PeerConnectionDetails::Asymmetric(_) => match hole_puncher {
                Some(mut puncher) => {
                    let socket_address = puncher.hole_punch_peer_without_port(remote_ip).await?;
                    // Wait for them to connect to us
                    Ok((None, socket_address))
                }
                None => Err(anyhow!("We have asymmetric nat but no local socket")),
            },
            PeerConnectionDetails::NoNat(socket_address) => {
                // They are symmetric (hard), we have no nat
                // Wait for them to connect
                Ok((None, socket_address))
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
                            (
                                Some(DiscoveredPeer {
                                    socket_address,
                                    socket_option: None,
                                    public_key: remote.public_key,
                                    topic: None,
                                }),
                                socket_address,
                            )
                        } else {
                            (None, socket_address)
                        })
                    }
                    None => Err(anyhow!("We have asymmetric nat but no local socket")),
                },
                PeerConnectionDetails::Symmetric(_) => {
                    let (socket, socket_address) = birthday_hard_side(socket_address).await?;
                    Ok((
                        Some(DiscoveredPeer {
                            socket_address,
                            socket_option: Some(socket),
                            public_key: remote.public_key,
                            topic: None,
                        }),
                        socket_address,
                    ))
                }
                PeerConnectionDetails::NoNat(socket_address) => {
                    // They are Asymmetric (easy), we have no nat
                    // just wait for them to connect
                    Ok((None, socket_address))
                }
            }
        }
        PeerConnectionDetails::NoNat(socket_address) => {
            // They have no nat - should be able to connect to them normally
            match local {
                PeerConnectionDetails::NoNat(our_socket_address) => {
                    // Need to decide whether to connect
                    Ok(if our_socket_address > socket_address {
                        (
                            Some(DiscoveredPeer {
                                socket_address,
                                socket_option: None,
                                public_key: remote.public_key,
                                topic: None,
                            }),
                            socket_address,
                        )
                    } else {
                        (None, socket_address)
                    })
                }
                PeerConnectionDetails::Symmetric(_) => {
                    let socket = UdpSocket::bind("0.0.0.0:0").await?;
                    Ok((
                        Some(DiscoveredPeer {
                            socket_address,
                            socket_option: Some(socket),
                            public_key: remote.public_key,
                            topic: None,
                        }),
                        socket_address,
                    ))
                }
                _ => Ok((
                    Some(DiscoveredPeer {
                        socket_address,
                        socket_option: None,
                        public_key: remote.public_key,
                        topic: None,
                    }),
                    socket_address,
                )),
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
