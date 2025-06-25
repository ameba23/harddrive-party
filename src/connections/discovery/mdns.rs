//! Peer discovery on local network using mDNS
use super::{DiscoveredPeer, DiscoveryMethod};
use anyhow::anyhow;
use harddrive_party_shared::wire_messages::AnnounceAddress;
use log::{debug, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    cmp::min,
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    sync::{Arc, RwLock},
};
use tokio::sync::mpsc::Sender;

/// Name of the mDNS service
const SERVICE_TYPE: &str = "_hdp._udp.local.";

/// Used when giving the public key as a property of a [ServiceInfo]
const ANNOUNCE_ADDRESS_PROPERTY_NAME: &str = "hdp-aa";

/// Announces ourself on mDNS
pub struct MdnsServer {}

impl MdnsServer {
    pub async fn new(
        id: &str,
        addr: SocketAddr,
        peers_tx: Sender<DiscoveredPeer>,
        announce_address: AnnounceAddress,
        known_peers: Arc<RwLock<HashSet<String>>>,
    ) -> anyhow::Result<Self> {
        let mdns_server = Self {};

        mdns_server.run(id, addr, peers_tx, announce_address, known_peers)?;
        Ok(mdns_server)
    }

    fn run(
        &self,
        id: &str,
        addr: SocketAddr,
        peers_tx: Sender<DiscoveredPeer>,
        announce_address: AnnounceAddress,
        known_peers: Arc<RwLock<HashSet<String>>>,
    ) -> anyhow::Result<()> {
        let mdns = ServiceDaemon::new()?;

        let mdns_receiver = mdns.browse(SERVICE_TYPE)?;

        let service = create_service_info(id, &addr, announce_address)?;
        mdns.register(service)?;

        tokio::spawn(async move {
            while let Ok(event) = mdns_receiver.recv_async().await {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        match parse_peer_info(info) {
                            Ok((their_addr, their_announce_address)) => {
                                if their_addr == addr {
                                    debug!("Found ourself on mdns");
                                } else {
                                    debug!("Found peer on mdns {their_addr:?}");

                                    {
                                        let mut known_peers = known_peers.write().unwrap();
                                        known_peers.insert(their_announce_address.name.clone());
                                    }

                                    // Only connect if our address is lexicographicaly greater than
                                    // theirs - to prevent duplicate connections
                                    let us = addr.to_string();
                                    let them = their_addr.to_string();
                                    if us > them
                                        && peers_tx
                                            .send(DiscoveredPeer {
                                                discovery_method: DiscoveryMethod::Mdns,
                                                announce_address: their_announce_address,
                                                socket_address: their_addr,
                                                socket_option: None,
                                            })
                                            .await
                                            .is_err()
                                    {
                                        warn!("Cannot send - peer discovery channel closed");
                                    }
                                }
                            }
                            Err(error) => {
                                warn!("Invalid mdns peer found {:?}", error);
                            }
                        }
                    }
                    ServiceEvent::ServiceRemoved(_type, fullname) => {
                        debug!("mdns peer removed {:?}", &fullname);
                    }
                    _ => {}
                }
            }
        });

        Ok(())
    }
}

/// Create an MDNS service with capabilities from the currently connected topics as properties
fn create_service_info(
    id: &str,
    addr: &SocketAddr,
    annouce_address: AnnounceAddress,
) -> anyhow::Result<ServiceInfo> {
    // Create a service info.
    let host_name = "localhost"; // TODO
    let mut properties = HashMap::new();
    // TODO here we could replace hex-encoded public key with announce address
    properties.insert(
        ANNOUNCE_ADDRESS_PROPERTY_NAME.to_string(),
        annouce_address.to_string(),
    );

    if let IpAddr::V4(ipv4_addr) = addr.ip() {
        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            &id[0..min(16, id.len())],
            host_name,
            ipv4_addr,
            addr.port(), //+ 150, // TODO
            Some(properties),
        )?;
        Ok(service_info)
    } else {
        // TODO if we bump mdns-sd we can handle ipv6 addresses
        Err(anyhow!("ipv6 address cannot be used for MDNS"))
    }
}

/// Handle a discovered [ServiceInfo] from a remote peer
fn parse_peer_info(info: ServiceInfo) -> anyhow::Result<(SocketAddr, AnnounceAddress)> {
    if info.get_type() != SERVICE_TYPE {
        return Err(anyhow!("Peer does not have expected service type"));
    }

    let properties = info.get_properties();

    let announce_address = properties
        .get(ANNOUNCE_ADDRESS_PROPERTY_NAME)
        .ok_or_else(|| anyhow!("Cannot get announce address property from mDNS service"))?;

    let announce_address = AnnounceAddress::from_string(announce_address.to_string())?;

    let their_ip = info
        .get_addresses()
        .iter()
        .next()
        .ok_or_else(|| anyhow!("Cannot get IP from discovered mDNS service info"))?;

    let their_port = info.get_port();

    let addr = SocketAddr::new(IpAddr::V4(*their_ip), their_port);
    Ok((addr, announce_address))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harddrive_party_shared::wire_messages::PeerConnectionDetails;
    use tokio::sync::mpsc::{channel, Receiver};

    async fn create_test_server(
        name: &str,
        socket_address: SocketAddr,
        annouce_address: AnnounceAddress,
    ) -> (MdnsServer, Receiver<DiscoveredPeer>) {
        let (peers_tx, peers_rx) = channel(1024);
        let server = MdnsServer::new(
            name,
            socket_address,
            peers_tx,
            annouce_address,
            Default::default(),
        )
        .await
        .unwrap();
        (server, peers_rx)
    }

    #[tokio::test]
    async fn test_mdns() {
        let _ = env_logger::builder().is_test(true).try_init();

        let local_ip = local_ip_address::local_ip().unwrap();
        let alice_socket_address = SocketAddr::new(local_ip, 1234);
        let bob_socket_address = SocketAddr::new(local_ip, 5678);
        let alice_announce = AnnounceAddress {
            connection_details: PeerConnectionDetails::NoNat("127.0.0.1:1234".parse().unwrap()),
            name: "BubblingBeaver".to_string(),
        };
        let bob_announce = AnnounceAddress {
            connection_details: PeerConnectionDetails::NoNat("127.0.0.1:1234".parse().unwrap()),
            name: "AngryAadvark".to_string(),
        };
        let (_alice, _alice_peers_rx) =
            create_test_server("alice", alice_socket_address, alice_announce.clone()).await;
        let (_bob, mut bob_peers_rx) =
            create_test_server("bob", bob_socket_address, bob_announce).await;

        let discovered_peer = bob_peers_rx.recv().await.unwrap();
        assert_eq!(discovered_peer.socket_address, alice_socket_address);
        assert_eq!(discovered_peer.discovery_method, DiscoveryMethod::Mdns);
        assert_eq!(discovered_peer.announce_address, alice_announce);
    }
}
