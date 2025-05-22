//! Peer discovery
use self::{
    hole_punch::{birthday_hard_side, PunchingUdpSocket},
    mdns::MdnsServer,
    stun::stun_test,
};
use crate::{
    peer::Peer,
    wire_messages::{AnnounceAddress, AnnouncePeer},
};
use anyhow::anyhow;
use base64::prelude::{Engine as _, BASE64_STANDARD_NO_PAD};
use harddrive_party_shared::wire_messages::PeerConnectionDetails;
use hole_punch::HolePuncher;
use local_ip_address::local_ip;
use log::{debug, error, warn};
use quinn::AsyncUdpSocket;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, RwLock},
};
use tokio::{
    net::UdpSocket,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Mutex,
    },
};

pub mod hole_punch;
pub mod mdns;
pub mod stun;

/// Details of a peer found through one of the discovery methods
#[derive(Debug)]
pub struct DiscoveredPeer {
    pub socket_address: SocketAddr,
    pub socket_option: Option<UdpSocket>,
    pub public_key: [u8; 32],
    pub discovery_method: DiscoveryMethod,
}

#[derive(Debug, Clone)]
pub enum DiscoveryMethod {
    Direct {
        announce_address: AnnounceAddress,
        request_id: u32,
    },
    Gossip {
        announce_address: AnnounceAddress,
    },
    Mdns,
}

impl DiscoveryMethod {
    pub fn get_announce_address(&self) -> Option<AnnounceAddress> {
        match self {
            DiscoveryMethod::Direct {
                announce_address, ..
            } => Some(announce_address.clone()),
            DiscoveryMethod::Gossip {
                announce_address, ..
            } => Some(announce_address.clone()),
            _ => None,
        }
    }

    pub fn get_request_id(&self) -> Option<u32> {
        match self {
            DiscoveryMethod::Direct { request_id, .. } => Some(*request_id),
            _ => None,
        }
    }
}

/// Handles the different peer discovery methods
pub struct PeerDiscovery {
    peers_tx: Sender<DiscoveredPeer>,
    pub peers_rx: Receiver<DiscoveredPeer>,
    hole_puncher: Option<HolePuncher>,
    /// Our own connection details
    announce_address: AnnounceAddress,
    pending_peer_connections: Arc<RwLock<HashMap<SocketAddr, DiscoveryMethod>>>,
    pub peer_announce_tx: Sender<AnnouncePeer>,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
}

impl PeerDiscovery {
    pub async fn new(
        // Whether to use mDNS
        use_mdns: bool,
        public_key: [u8; 32],
        peers: Arc<Mutex<HashMap<String, Peer>>>,
    ) -> anyhow::Result<(Option<PunchingUdpSocket>, Self)> {
        // Channel for reporting discovered peers
        let (peers_tx, peers_rx) = channel(1024);

        // Channel for announcing peers to be handled
        let (peer_announce_tx, mut peer_announce_rx) = channel(1024);

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
        let _mdns_server = if use_mdns && is_private(my_local_ip) {
            Some(MdnsServer::new(&id, addr, peers_tx.clone(), public_key).await?)
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

        let peer_discovery = Self {
            peers_tx: peers_tx.clone(),
            peers_rx,
            hole_puncher: hole_puncher.clone(),
            announce_address,
            pending_peer_connections: Default::default(),
            peer_announce_tx,
            peers,
        };

        let pending_peer_connections = peer_discovery.pending_peer_connections.clone();
        let own_announce_address = peer_discovery.announce_address.clone();
        let peers = peer_discovery.peers.clone();

        // In a separate task, loop over gossiped peer announcements
        tokio::spawn(async move {
            while let Some(announce_peer) = peer_announce_rx.recv().await {
                if let Err(err) = handle_peer_announcement(
                    hole_puncher.clone(),
                    own_announce_address.clone(),
                    peers_tx.clone(),
                    pending_peer_connections.clone(),
                    peers.clone(),
                    DiscoveryMethod::Gossip {
                        announce_address: announce_peer.announce_address,
                    },
                )
                .await
                {
                    warn!("Failed to handle gossiped peer announcement {err}");
                }
            }
        });

        Ok((socket_option, peer_discovery))
    }

    pub async fn connect_direct_to_peer(
        &mut self,
        announce_payload: &str,
        request_id: u32,
    ) -> anyhow::Result<()> {
        let announce_address_bytes = BASE64_STANDARD_NO_PAD.decode(announce_payload)?;
        let announce_address = AnnounceAddress::from_bytes(announce_address_bytes)?;

        handle_peer_announcement(
            self.hole_puncher.clone(),
            self.announce_address.clone(),
            self.peers_tx.clone(),
            self.pending_peer_connections.clone(),
            self.peers.clone(),
            DiscoveryMethod::Direct {
                announce_address: announce_address.clone(),
                request_id,
            },
        )
        .await
    }

    pub fn get_pending_peer(&self, socket_address: &SocketAddr) -> Option<DiscoveryMethod> {
        if let Ok(mut connections) = self.pending_peer_connections.write() {
            connections.remove(socket_address)
        } else {
            error!("Poisoned RwLock pending_peer_connections");
            None
        }
    }

    pub fn get_ui_announce_address(&self) -> anyhow::Result<String> {
        let bytes = self.announce_address.to_bytes();
        Ok(BASE64_STANDARD_NO_PAD.encode(&bytes))
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

pub async fn handle_peer_announcement(
    hole_puncher: Option<HolePuncher>,
    our_announce_address: AnnounceAddress,
    peers_tx: Sender<DiscoveredPeer>,
    pending_peer_connections: Arc<RwLock<HashMap<SocketAddr, DiscoveryMethod>>>,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    discovery_method: DiscoveryMethod,
) -> anyhow::Result<()> {
    let announce_address = discovery_method
        .get_announce_address()
        .ok_or(anyhow!("Cannot handle MDNS peer"))?
        .clone();

    // Check it is not ourself
    if our_announce_address == announce_address {
        return Ok(());
    }

    // Check that we are not already connected to this peer
    let name = key_to_animal::key_to_name(&announce_address.public_key);
    if peers.lock().await.contains_key(&name) {
        return Ok(());
    }

    // TODO check that it is not already a pending peer connection
    debug!("Remote peer {:?}", announce_address);
    return match handle_peer(
        hole_puncher.clone(),
        &our_announce_address.connection_details,
        discovery_method.clone(),
    )
    .await
    {
        Ok((Some(discovered_peer), _)) => {
            // We connect to them
            debug!("Connect to {:?}", discovered_peer);
            if peers_tx.send(discovered_peer).await.is_err() {
                error!("Cannot write to channel");
            }
            Ok(())
        }
        Ok((None, socket_address)) => {
            debug!("Successfully handled peer - awaiting connection from their side");
            // They connect to us
            pending_peer_connections
                .write()
                .map_err(|_| anyhow!("Poisoned RwLock pending_peer_connections"))?
                .insert(socket_address, discovery_method);
            Ok(())
        }
        Err(error) => Err(anyhow!("Error when handling discovered peer {:?}", error)),
    };
}

pub async fn handle_peer(
    hole_puncher: Option<HolePuncher>,
    local: &PeerConnectionDetails,
    discovery_method: DiscoveryMethod,
) -> anyhow::Result<(Option<DiscoveredPeer>, SocketAddr)> {
    let announce_address = discovery_method
        .get_announce_address()
        .ok_or(anyhow!("Cannot handle MDNS peer"))?
        .clone();
    match announce_address.connection_details {
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
                Ok((None, *socket_address))
            }
        },
        PeerConnectionDetails::Asymmetric(socket_address) => {
            match local {
                PeerConnectionDetails::Asymmetric(our_socket_address) => {
                    match hole_puncher {
                        Some(mut puncher) => {
                            if our_socket_address.ip() != socket_address.ip() {
                                puncher.hole_punch_peer(socket_address).await?;
                            }
                            // Decide whether to connect or let them connect, by lexicographically
                            // comparing socket addresses
                            Ok(if our_socket_address > &socket_address {
                                (
                                    Some(DiscoveredPeer {
                                        discovery_method,
                                        socket_address,
                                        socket_option: None,
                                        public_key: announce_address.public_key,
                                    }),
                                    socket_address,
                                )
                            } else {
                                (None, socket_address)
                            })
                        }
                        None => Err(anyhow!("We have asymmetric nat but no local socket")),
                    }
                }
                PeerConnectionDetails::Symmetric(_) => {
                    let (socket, socket_address) = birthday_hard_side(socket_address).await?;
                    Ok((
                        Some(DiscoveredPeer {
                            discovery_method,
                            socket_address,
                            socket_option: Some(socket),
                            public_key: announce_address.public_key,
                        }),
                        socket_address,
                    ))
                }
                PeerConnectionDetails::NoNat(socket_address) => {
                    // They are Asymmetric (easy), we have no nat
                    // just wait for them to connect
                    Ok((None, *socket_address))
                }
            }
        }
        PeerConnectionDetails::NoNat(socket_address) => {
            // They have no nat - should be able to connect to them normally
            match local {
                PeerConnectionDetails::NoNat(our_socket_address) => {
                    // Need to decide whether to connect
                    Ok(if our_socket_address > &socket_address {
                        (
                            Some(DiscoveredPeer {
                                discovery_method,
                                socket_address,
                                socket_option: None,
                                public_key: announce_address.public_key,
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
                            discovery_method,
                            socket_address,
                            socket_option: Some(socket),
                            public_key: announce_address.public_key,
                        }),
                        socket_address,
                    ))
                }
                _ => Ok((
                    Some(DiscoveredPeer {
                        discovery_method,
                        socket_address,
                        socket_option: None,
                        public_key: announce_address.public_key,
                    }),
                    socket_address,
                )),
            }
        }
    }
}
