//! Peer discovery
use self::{
    hole_punch::{birthday_hard_side, PunchingUdpSocket},
    mdns::MdnsServer,
    stun::stun_test,
};
use crate::{
    connections::known_peers::KnownPeers, errors::UiServerErrorWrapper, peer::Peer,
    wire_messages::AnnounceAddress,
};
use harddrive_party_shared::{ui_messages::UiServerError, wire_messages::PeerConnectionDetails};
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
        oneshot, Mutex,
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
    pub discovery_method: DiscoveryMethod,
    pub announce_address: AnnounceAddress,
}

/// The way by which a peer was discovered
#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveryMethod {
    /// User provided the announce address
    Direct,
    /// Another peer provided the announce address
    Gossip,
    /// Discovered over local network
    Mdns,
}

/// Handles the different peer discovery methods
pub struct PeerDiscovery {
    pub peers_rx: Receiver<DiscoveredPeer>,
    /// Our own connection details
    pub announce_address: AnnounceAddress,
    pending_peer_connections: Arc<RwLock<HashMap<SocketAddr, (DiscoveryMethod, AnnounceAddress)>>>,
    /// Channel used to announce peers
    pub peer_announce_tx: Sender<PeerConnect>,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    pub known_peers: KnownPeers,
}

impl PeerDiscovery {
    pub async fn new(
        // Whether to use mDNS
        use_mdns: bool,
        public_key: [u8; 32],
        peers: Arc<Mutex<HashMap<String, Peer>>>,
        port: Option<u16>,
        known_peers_db: sled::Tree,
    ) -> anyhow::Result<(Option<PunchingUdpSocket>, Self)> {
        // Channel for reporting discovered peers
        let (peers_tx, peers_rx) = channel(1024);

        // Channel for announcing peers to be handled
        let (peer_announce_tx, mut peer_announce_rx) = channel(1024);

        let my_local_ip = local_ip()?;

        let raw_socket = if let Some(given_port) = port {
            // If we get an error with a given port try again with the port set to 0
            if let Ok(socket) = UdpSocket::bind(SocketAddr::new(my_local_ip, given_port)).await {
                socket
            } else {
                UdpSocket::bind(SocketAddr::new(my_local_ip, 0)).await?
            }
        } else {
            UdpSocket::bind(SocketAddr::new(my_local_ip, 0)).await?
        };

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
        let id = hex::encode(public_key);
        let known_peers = KnownPeers::new(known_peers_db);

        let socket_option = match local_connection_details {
            // TODO probably need to give the ip address here
            PeerConnectionDetails::Symmetric(_) => None,
            _ => Some(socket),
        };

        let announce_address = AnnounceAddress {
            connection_details: local_connection_details.clone(),
            name: key_to_animal::key_to_name(&public_key),
        };

        // Only use mdns if we are on a local network
        let _mdns_server = if use_mdns && is_private(my_local_ip) {
            Some(
                MdnsServer::new(
                    &id,
                    addr,
                    peers_tx.clone(),
                    announce_address.clone(),
                    known_peers.clone(),
                )
                .await?,
            )
        } else {
            None
        };

        let peer_discovery = Self {
            peers_rx,
            announce_address,
            pending_peer_connections: Default::default(),
            peer_announce_tx,
            peers,
            known_peers: known_peers.clone(),
        };

        let pending_peer_connections = peer_discovery.pending_peer_connections.clone();
        let own_announce_address = peer_discovery.announce_address.clone();
        let peers = peer_discovery.peers.clone();

        // In a separate task, loop over peer announcements
        tokio::spawn(async move {
            while let Some(peer_connect) = peer_announce_rx.recv().await {
                debug!(
                    "Attempting to connect to peer {}",
                    peer_connect.announce_address
                );
                let result = handle_peer_announcement(
                    hole_puncher.clone(),
                    own_announce_address.clone(),
                    peers_tx.clone(),
                    pending_peer_connections.clone(),
                    peers.clone(),
                    peer_connect.announce_address,
                    peer_connect.discovery_method,
                    known_peers.clone(),
                )
                .await;

                if let Some(response_tx) = peer_connect.response_tx {
                    let _ = response_tx.send(result);
                } else if let Err(err) = result {
                    warn!("Failed to handle gossiped peer announcement {err}");
                }
            }
        });

        Ok((socket_option, peer_discovery))
    }

    pub fn get_pending_peer(
        &self,
        socket_address: &SocketAddr,
    ) -> Option<(DiscoveryMethod, AnnounceAddress)> {
        if let Ok(mut connections) = self.pending_peer_connections.write() {
            connections.remove(socket_address)
        } else {
            // TODO can clear poison
            error!("Poisoned RwLock pending_peer_connections");
            None
        }
    }

    /// Decide whether to use client verification based on our NAT type
    ///
    /// If we are not behind NAT we allow connections without needing to
    /// know their announce address (containing public key) up front
    pub fn use_client_verification(&self) -> bool {
        !matches!(
            self.announce_address.connection_details,
            PeerConnectionDetails::NoNat(_)
        )
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

/// This is called when a peer is announced, either directly by the user or through a gossiped peer
/// announcement from another connected peer.
#[allow(clippy::too_many_arguments)]
pub async fn handle_peer_announcement(
    hole_puncher: Option<HolePuncher>,
    our_announce_address: AnnounceAddress,
    peers_tx: Sender<DiscoveredPeer>,
    pending_peer_connections: Arc<RwLock<HashMap<SocketAddr, (DiscoveryMethod, AnnounceAddress)>>>,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    their_announce_address: AnnounceAddress,
    discovery_method: DiscoveryMethod,
    known_peers: KnownPeers,
) -> Result<(), UiServerErrorWrapper> {
    known_peers.add_peer(&their_announce_address)?;
    // Check it is not ourself
    if our_announce_address == their_announce_address {
        return Err(UiServerError::PeerDiscovery("Cannot connect to ourself".to_string()).into());
    }

    // Check that we are not already connected to this peer
    if peers
        .lock()
        .await
        .contains_key(&their_announce_address.name)
    {
        return Err(
            UiServerError::PeerDiscovery("Already connected to this peer".to_string()).into(),
        );
    }

    // TODO check that it is not already a pending peer connection
    debug!("Remote peer {their_announce_address:?}");
    return match handle_peer(
        hole_puncher.clone(),
        &our_announce_address.connection_details,
        their_announce_address.clone(),
        discovery_method.clone(),
    )
    .await?
    {
        (Some(discovered_peer), _) => {
            // We connect to them
            debug!("Connecting to {discovered_peer:?}");
            if peers_tx.send(discovered_peer).await.is_err() {
                error!("Cannot write to channel");
            }
            Ok(())
        }
        (None, socket_address) => {
            debug!("Successfully handled peer - awaiting connection from their side");
            // TODO should we clear poison here?
            // They connect to us
            pending_peer_connections
                .write()?
                .insert(socket_address, (discovery_method, their_announce_address));
            Ok(())
        }
    };
}

/// Handle a peer we have discovered - depending on NAT type
pub async fn handle_peer(
    hole_puncher: Option<HolePuncher>,
    local: &PeerConnectionDetails,
    announce_address: AnnounceAddress,
    discovery_method: DiscoveryMethod,
) -> Result<(Option<DiscoveredPeer>, SocketAddr), UiServerErrorWrapper> {
    match announce_address.connection_details {
        PeerConnectionDetails::Symmetric(remote_ip) => match local {
            PeerConnectionDetails::Symmetric(_) => Err(UiServerError::PeerDiscovery(
                "Symmetric to symmetric not yet supported".to_string(),
            )
            .into()),
            PeerConnectionDetails::Asymmetric(_) => match hole_puncher {
                Some(mut puncher) => {
                    let socket_address = puncher.hole_punch_peer_without_port(remote_ip).await?;
                    // Wait for them to connect to us
                    Ok((None, socket_address))
                }
                None => Err(UiServerError::PeerDiscovery(
                    "We have asymmetric NAT but no local socket".to_string(),
                )
                .into()),
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
                                        announce_address,
                                        socket_address,
                                        socket_option: None,
                                    }),
                                    socket_address,
                                )
                            } else {
                                (None, socket_address)
                            })
                        }
                        None => Err(UiServerError::PeerDiscovery(
                            "We have asymmetric nat but no local socket".to_string(),
                        )
                        .into()),
                    }
                }
                PeerConnectionDetails::Symmetric(_) => {
                    let (socket, socket_address) = birthday_hard_side(socket_address).await?;
                    Ok((
                        Some(DiscoveredPeer {
                            discovery_method,
                            announce_address,
                            socket_address,
                            socket_option: Some(socket),
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
                                announce_address,
                                socket_address,
                                socket_option: None,
                            }),
                            socket_address,
                        )
                    } else {
                        (None, socket_address)
                    })
                }
                PeerConnectionDetails::Symmetric(_) => {
                    let socket = UdpSocket::bind("0.0.0.0:0")
                        .await
                        .map_err(|e| UiServerError::PeerDiscovery(e.to_string()))?;
                    Ok((
                        Some(DiscoveredPeer {
                            discovery_method,
                            announce_address,
                            socket_address,
                            socket_option: Some(socket),
                        }),
                        socket_address,
                    ))
                }
                _ => Ok((
                    Some(DiscoveredPeer {
                        discovery_method,
                        announce_address,
                        socket_address,
                        socket_option: None,
                    }),
                    socket_address,
                )),
            }
        }
    }
}

pub struct PeerConnect {
    pub discovery_method: DiscoveryMethod,
    pub announce_address: AnnounceAddress,
    pub response_tx: Option<oneshot::Sender<Result<(), UiServerErrorWrapper>>>,
}
