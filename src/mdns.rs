use anyhow::anyhow;
use log::{debug, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::{IpAddr, SocketAddr};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

const SERVICE_TYPE: &str = "_hdp._udp.local.";
const TOPIC: &str = "topic";
const PORT: &str = "port";

/// A peer discovered by mdns
pub struct MdnsPeerInfo {
    pub addr: SocketAddr,
    pub topic: String,
}

impl MdnsPeerInfo {
    fn new(info: ServiceInfo) -> anyhow::Result<Self> {
        if info.get_type() != SERVICE_TYPE {
            return Err(anyhow!("Peer does not have expected service type"));
        }

        let properties = info.get_properties();

        let topic = properties
            .get(&TOPIC.to_string())
            .ok_or_else(|| anyhow!("Cannot get topic"))?
            .to_string();

        let their_ip = info
            .get_addresses()
            .iter()
            .next()
            .ok_or_else(|| anyhow!("Cannot get ip"))?;

        let their_port = properties
            .get(&PORT.to_string())
            .ok_or_else(|| anyhow!("Cannot get port"))?
            .parse::<u16>()?;

        let addr = SocketAddr::new(IpAddr::V4(*their_ip), their_port);
        Ok(Self { addr, topic })
    }
}

/// Announce ourself on mdns and discover other local peers
pub async fn mdns_server(
    name: &str,
    addr: SocketAddr,
    topic: String,
) -> anyhow::Result<UnboundedReceiver<MdnsPeerInfo>> {
    let (peers_tx, peers_rx) = unbounded_channel();
    let mdns = ServiceDaemon::new()?;

    // Create a service info.
    let host_name = "localhost"; // TODO
    let mut properties = std::collections::HashMap::new();
    properties.insert(TOPIC.to_string(), topic.clone());
    properties.insert(PORT.to_string(), addr.port().to_string());

    if let IpAddr::V4(ipv4_addr) = addr.ip() {
        let my_service = ServiceInfo::new(
            SERVICE_TYPE,
            name,
            host_name,
            ipv4_addr,
            addr.port() + 150,
            Some(properties),
        )?;
        // .enable_addr_auto();

        // Register with the daemon, which publishes the service.
        mdns.register(my_service)?;
        // std::thread::sleep(std::time::Duration::from_secs(5));
        // let monitor = mdns.monitor().expect("Failed to monitor the daemon");
        // // Only do this if we monitor the daemon events, which is optional.
        // while let Ok(event) = monitor.recv() {
        //     println!("Daemon event: {:?}", &event);
        // }

        let mdns_receiver = mdns.browse(SERVICE_TYPE)?;

        tokio::spawn(async move {
            // Receive the browse events in sync or async. Here is
            // an example of using a thread. Users can call `receiver.recv_async().await`
            // if running in async environment.
            while let Ok(event) = mdns_receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        debug!("Resolved a mdns service: {:?}", info);
                        match MdnsPeerInfo::new(info) {
                            Ok(mdns_peer) => {
                                if mdns_peer.topic == topic {
                                    if mdns_peer.addr == addr {
                                        debug!("Found ourself on mdns");
                                    } else {
                                        // Only connect if our address is lexicographicaly greater than
                                        // theirs - to prevent duplicate connections
                                        let us = addr.to_string();
                                        let them = mdns_peer.addr.to_string();
                                        if us > them && peers_tx.send(mdns_peer).is_err() {
                                            warn!("Cannot send - mdns peer channel closed");
                                        }
                                    }
                                } else {
                                    warn!("Found peer with unknown mdns topic");
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
        Ok(peers_rx)
    } else {
        Err(anyhow!("ipv6 address cannot be used for MDNS"))
    }
}
