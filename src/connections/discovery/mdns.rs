//! Peer discovery on local network using mDNS
use crate::connections::known_peers::KnownPeers;

use super::{DiscoveredPeer, DiscoveryMethod};
use harddrive_party_shared::wire_messages::AnnounceAddress;
use log::{debug, error, warn};
use mdns_sd::{ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    cmp::min,
    collections::HashMap,
    net::{IpAddr, SocketAddr, SocketAddrV6},
};
use tokio::sync::mpsc::Sender;

/// Name of the mDNS service
const SERVICE_TYPE: &str = "_hdp._udp.local.";

/// Used when giving the announce address as a property of a [ServiceInfo]
const ANNOUNCE_ADDRESS_PROPERTY_NAME: &str = "hdp-aa";

/// Announces ourself on mDNS
pub struct MdnsServer {}

impl MdnsServer {
    pub async fn new(
        id: &str,
        addr: SocketAddr,
        peers_tx: Sender<DiscoveredPeer>,
        announce_address: AnnounceAddress,
        known_peers: KnownPeers,
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
        known_peers: KnownPeers,
    ) -> anyhow::Result<()> {
        let mdns = ServiceDaemon::new()?;

        let mdns_receiver = mdns.browse(SERVICE_TYPE)?;

        let service = create_service_info(id, &addr, announce_address)?;
        mdns.register(service)?;

        tokio::spawn(async move {
            while let Ok(event) = mdns_receiver.recv_async().await {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        match parse_peer_info(&info, &addr) {
                            Ok((their_addr, their_announce_address)) => {
                                if their_addr == addr {
                                    debug!("Found ourself on mdns");
                                } else {
                                    debug!("Found peer on mdns {their_addr:?}");

                                    if let Err(err) = known_peers.add_peer(&their_announce_address)
                                    {
                                        error!("Unable to add peer to known-peers db: {err}");
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
                                warn!("Invalid mdns peer found {error:?}");
                            }
                        }
                    }
                    ServiceEvent::ServiceRemoved(_type, fullname) => {
                        debug!("mdns peer removed {:?}", fullname);
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
    let instance_name = &id[0..min(16, id.len())];
    // Both instance name and hostname are derived from the peer's public key,
    // so collisions are not possible. Skipping the RFC 6762 §8 probe avoids the
    // 750ms delay before our records hit the wire — without it, a peer that
    // started first can initiate an inbound connection before our discovery
    // loop has populated known_peers, causing a TLS UnknownIssuer rejection.
    let host_name = format!("{instance_name}.local.");
    let mut properties = HashMap::new();
    properties.insert(
        ANNOUNCE_ADDRESS_PROPERTY_NAME.to_string(),
        annouce_address.to_string(),
    );

    let mut service_info = ServiceInfo::new(
        SERVICE_TYPE,
        instance_name,
        &host_name,
        addr.ip(),
        addr.port(),
        Some(properties),
    )?;
    service_info.set_requires_probe(false);
    Ok(service_info)
}

/// Handle a discovered [ResolvedService] from a remote peer
fn parse_peer_info(
    info: &ResolvedService,
    our_addr: &SocketAddr,
) -> anyhow::Result<(SocketAddr, AnnounceAddress)> {
    if info.ty_domain.as_str() != SERVICE_TYPE {
        anyhow::bail!("Peer does not have expected service type");
    }

    let announce_address = info
        .get_property_val_str(ANNOUNCE_ADDRESS_PROPERTY_NAME)
        .ok_or_else(|| anyhow::anyhow!("Cannot get announce address property from mDNS service"))?;

    let announce_address = AnnounceAddress::from_string(announce_address.to_string())?;

    let their_port = info.get_port();

    let addresses = info.get_addresses();
    let chosen = addresses
        .iter()
        .find(|ip| ip.is_ipv4() == our_addr.is_ipv4())
        .or_else(|| addresses.iter().next())
        .ok_or_else(|| anyhow::anyhow!("Cannot get IP from discovered mDNS service info"))?;
    let addr = socket_addr_from_scoped_ip(chosen, their_port);
    Ok((addr, announce_address))
}

fn socket_addr_from_scoped_ip(ip: &ScopedIp, port: u16) -> SocketAddr {
    match ip {
        ScopedIp::V4(ipv4_addr) => SocketAddr::new(IpAddr::V4(*ipv4_addr.addr()), port),
        ScopedIp::V6(ipv6_addr) => SocketAddr::V6(SocketAddrV6::new(
            *ipv6_addr.addr(),
            port,
            0,
            ipv6_addr.scope_id().index,
        )),
        _ => SocketAddr::new(ip.to_ip_addr(), port),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harddrive_party_shared::wire_messages::PeerConnectionDetails;
    use std::collections::HashSet;
    use tempfile::TempDir;
    use tokio::sync::mpsc::{channel, Receiver};

    fn announce_address(addr: SocketAddr, name: &str) -> AnnounceAddress {
        let mut key = [0; 32];
        for (index, byte) in name.bytes().enumerate() {
            key[index % key.len()] ^= byte;
        }
        AnnounceAddress {
            connection_details: PeerConnectionDetails::NoNat(addr),
            public_key: harddrive_party_shared::PeerId::new(key),
        }
    }

    async fn create_test_server(
        name: &str,
        socket_address: SocketAddr,
        annouce_address: AnnounceAddress,
    ) -> (MdnsServer, Receiver<DiscoveredPeer>, KnownPeers) {
        let storage = TempDir::new().unwrap();
        let db = sled::open(storage).unwrap();
        let db = db.open_tree(b"k").unwrap();
        let known_peers = KnownPeers::new(db);

        let (peers_tx, peers_rx) = channel(1024);
        let server = MdnsServer::new(
            name,
            socket_address,
            peers_tx,
            annouce_address,
            known_peers.clone(),
        )
        .await
        .unwrap();
        (server, peers_rx, known_peers)
    }

    #[tokio::test]
    async fn test_mdns() {
        let _ = env_logger::builder().is_test(true).try_init();

        let local_ip = local_ip_address::local_ip().unwrap();
        let alice_socket_address = SocketAddr::new(local_ip, 1234);
        let bob_socket_address = SocketAddr::new(local_ip, 5678);
        let alice_announce = announce_address("127.0.0.1:1234".parse().unwrap(), "BubblingBeaver");
        let bob_announce = announce_address("127.0.0.1:1234".parse().unwrap(), "AngryAadvark");
        let (_alice, _alice_peers_rx, alice_known) =
            create_test_server("alice", alice_socket_address, alice_announce.clone()).await;
        let (_bob, mut bob_peers_rx, _bob_known) =
            create_test_server("bob", bob_socket_address, bob_announce.clone()).await;

        // The LAN may have other hdp peers advertising the same service type;
        // loop until we see Alice specifically, ignoring strangers.
        let discovered_peer = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let peer = bob_peers_rx.recv().await.unwrap();
                if peer.announce_address == alice_announce {
                    return peer;
                }
            }
        })
        .await
        .expect("Bob did not discover Alice within timeout");
        assert_eq!(discovered_peer.socket_address, alice_socket_address);
        assert_eq!(discovered_peer.discovery_method, DiscoveryMethod::Mdns);

        // Alice doesn't send to her channel due to the lex tie-break, but she
        // must still add Bob to known_peers — otherwise her TLS verifier rejects
        // Bob's incoming client cert with UnknownIssuer. A regression in this
        // pathway (e.g. a hostname conflict suppressing one peer's records)
        // would prevent Alice from ever seeing Bob.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !alice_known.has(&bob_announce.public_key) {
            if tokio::time::Instant::now() >= deadline {
                panic!("Alice did not learn of Bob via mDNS within timeout");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn parses_ipv6_service_info() {
        let socket_address: SocketAddr = "[fd00::1]:1234".parse().unwrap();
        let announce = announce_address(socket_address, "BubblingBeaver");

        let service_info = create_service_info("alice", &socket_address, announce.clone()).unwrap();
        let resolved_service = service_info.as_resolved_service();
        let (discovered_addr, discovered_announce) =
            parse_peer_info(&resolved_service, &socket_address).unwrap();

        assert_eq!(discovered_addr, socket_address);
        assert_eq!(discovered_announce, announce);
    }

    #[test]
    fn prefers_address_matching_our_family() {
        let their_v4: SocketAddr = "192.168.0.2:1234".parse().unwrap();
        let their_v6: SocketAddr = "[fd00::2]:1234".parse().unwrap();
        let announce = announce_address(their_v4, "BubblingBeaver");

        let service_info = create_service_info("alice", &their_v4, announce.clone()).unwrap();
        let mut resolved_service = service_info.as_resolved_service();
        resolved_service.addresses =
            HashSet::from([ScopedIp::from(their_v4.ip()), ScopedIp::from(their_v6.ip())]);

        let our_v4: SocketAddr = "192.168.0.1:1234".parse().unwrap();
        let (chosen, _) = parse_peer_info(&resolved_service, &our_v4).unwrap();
        assert_eq!(chosen, their_v4);

        let our_v6: SocketAddr = "[fd00::1]:1234".parse().unwrap();
        let (chosen, _) = parse_peer_info(&resolved_service, &our_v6).unwrap();
        assert_eq!(chosen, their_v6);
    }

    #[test]
    fn preserves_link_local_ipv6_scope_id() {
        use if_addrs::{IfAddr, IfOperStatus, Ifv6Addr, Interface};

        let socket_address: SocketAddr = "[fe80::1]:1234".parse().unwrap();
        let announce = announce_address(socket_address, "BubblingBeaver");
        let interface = Interface {
            name: "eth0".to_string(),
            addr: IfAddr::V6(Ifv6Addr {
                ip: "fe80::1".parse().unwrap(),
                netmask: "ffff:ffff:ffff:ffff::".parse().unwrap(),
                prefixlen: 64,
                broadcast: None,
            }),
            index: Some(7),
            oper_status: IfOperStatus::Up,
            is_p2p: false,
            #[cfg(windows)]
            adapter_name: "{00000000-0000-0000-0000-000000000000}".to_string(),
        };

        let service_info = create_service_info("alice", &socket_address, announce.clone()).unwrap();
        let mut resolved_service = service_info.as_resolved_service();
        resolved_service.addresses = HashSet::from([ScopedIp::from(&interface)]);

        let (discovered_addr, discovered_announce) =
            parse_peer_info(&resolved_service, &socket_address).unwrap();

        assert_eq!(discovered_addr.ip(), socket_address.ip());
        assert_eq!(discovered_addr.port(), socket_address.port());
        match discovered_addr {
            SocketAddr::V6(addr) => assert_eq!(addr.scope_id(), 7),
            SocketAddr::V4(_) => panic!("expected IPv6 address"),
        }
        assert_eq!(discovered_announce, announce);
    }
}
