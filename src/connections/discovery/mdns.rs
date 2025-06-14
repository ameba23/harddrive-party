//! Peer discovery on local network using mDNS
use super::{DiscoveredPeer, DiscoveryMethod};
use anyhow::anyhow;
use log::{debug, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    cmp::min,
    collections::HashMap,
    net::{IpAddr, SocketAddr},
};
use tokio::sync::mpsc::Sender;

/// Name of the mDNS service
const SERVICE_TYPE: &str = "_hdp._udp.local.";

/// Used when giving the public key as a property of a [ServiceInfo]
const PUBLIC_KEY_PROPERTY_NAME: &str = "hdp-pk";

/// Announces ourself on mDNS
pub struct MdnsServer {}

impl MdnsServer {
    pub async fn new(
        id: &str,
        addr: SocketAddr,
        peers_tx: Sender<DiscoveredPeer>,
        public_key: [u8; 32],
    ) -> anyhow::Result<Self> {
        let mdns_server = Self {};

        mdns_server.run(id, addr, peers_tx, public_key)?;
        Ok(mdns_server)
    }

    fn run(
        &self,
        id: &str,
        addr: SocketAddr,
        peers_tx: Sender<DiscoveredPeer>,
        public_key: [u8; 32],
    ) -> anyhow::Result<()> {
        let mdns = ServiceDaemon::new()?;

        let mdns_receiver = mdns.browse(SERVICE_TYPE)?;

        let service = create_service_info(id, &addr, &public_key)?;
        mdns.register(service)?;

        tokio::spawn(async move {
            while let Ok(event) = mdns_receiver.recv_async().await {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        match parse_peer_info(info) {
                            Ok((their_addr, their_public_key)) => {
                                if their_addr == addr {
                                    debug!("Found ourself on mdns");
                                } else {
                                    debug!("Found peer on mdns {their_addr:?}");
                                    // Only connect if our address is lexicographicaly greater than
                                    // theirs - to prevent duplicate connections
                                    let us = addr.to_string();
                                    let them = their_addr.to_string();
                                    if us > them
                                        && peers_tx
                                            .send(DiscoveredPeer {
                                                discovery_method: DiscoveryMethod::Mdns {
                                                    public_key: their_public_key,
                                                },
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
    public_key: &[u8; 32],
) -> anyhow::Result<ServiceInfo> {
    // Create a service info.
    let host_name = "localhost"; // TODO
    let mut properties = HashMap::new();
    properties.insert(
        PUBLIC_KEY_PROPERTY_NAME.to_string(),
        hex::encode(public_key),
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
fn parse_peer_info(info: ServiceInfo) -> anyhow::Result<(SocketAddr, [u8; 32])> {
    if info.get_type() != SERVICE_TYPE {
        return Err(anyhow!("Peer does not have expected service type"));
    }

    let properties = info.get_properties();

    let public_key = properties
        .get(PUBLIC_KEY_PROPERTY_NAME)
        .ok_or_else(|| anyhow!("Cannot get public key property from mDNS service"))?;

    let public_key = hex::decode(public_key)?
        .try_into()
        .map_err(|_| anyhow!("Bad public key length"))?;

    let their_ip = info
        .get_addresses()
        .iter()
        .next()
        .ok_or_else(|| anyhow!("Cannot get IP from discovered mDNS service info"))?;

    let their_port = info.get_port();

    let addr = SocketAddr::new(IpAddr::V4(*their_ip), their_port);
    Ok((addr, public_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::{channel, Receiver};

    async fn create_test_server(
        name: &str,
        socket_address: SocketAddr,
        public_key: [u8; 32],
    ) -> (MdnsServer, Receiver<DiscoveredPeer>) {
        let (peers_tx, peers_rx) = channel(1024);
        let server = MdnsServer::new(name, socket_address, peers_tx, public_key)
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
        let (_alice, _alice_peers_rx) =
            create_test_server("alice", alice_socket_address, [0; 32]).await;
        let (_bob, mut bob_peers_rx) = create_test_server("bob", bob_socket_address, [1; 32]).await;

        let discovered_peer = bob_peers_rx.recv().await.unwrap();
        assert_eq!(discovered_peer.socket_address, alice_socket_address);
        assert_eq!(
            discovered_peer.discovery_method,
            DiscoveryMethod::Mdns {
                public_key: [0; 32]
            }
        );
    }
}
