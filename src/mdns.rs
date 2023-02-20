use anyhow::anyhow;
use log::{debug, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::{IpAddr, SocketAddr};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

const SERVICE_TYPE: &str = "_hdp._udp.local.";
const TOPIC: &str = "topic";

/// A peer discovered by mdns
pub struct MdnsPeerInfo {
    pub addr: SocketAddr,
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

    if let IpAddr::V4(ipv4_addr) = addr.ip() {
        let my_service = ServiceInfo::new(
            SERVICE_TYPE,
            name,
            host_name,
            ipv4_addr,
            addr.port(),
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
                        //{ ty_domain: "_mdns-sd-my-test._udp.local.", sub_domain: None, fullname: "two._mdns-sd-my-test._udp.local.", server: "localhost.", addresses: {192.168.1.111}, port: 5200, host_ttl: 120, other_ttl: 4500, priority: 0, weight: 0, properties: {"property_1": "test", "property_2": "1234"}, last_update: 1676022866375 }
                        if info.get_type() == SERVICE_TYPE {
                            // Check topic property matches ours
                            match info.get_properties().get(&TOPIC.to_string()) {
                                Some(their_topic) => {
                                    if their_topic == &topic {
                                        // send a PeerInfo with ip and port
                                        // TODO handle multiple addresses and errors
                                        let ip = info.get_addresses().iter().next().unwrap();
                                        if ip == &ipv4_addr && info.get_port() == addr.port() {
                                            debug!("Found ourself on mdns");
                                        } else {
                                            let peer_info = MdnsPeerInfo {
                                                addr: SocketAddr::new(
                                                    IpAddr::V4(*ip),
                                                    info.get_port(),
                                                ),
                                            };

                                            // Only connect if our address is lexicographicaly greater than
                                            // theirs - to prevent duplicate connections
                                            let us = addr.to_string();
                                            let them = peer_info.addr.to_string();
                                            if us > them && peers_tx.send(peer_info).is_err() {
                                                warn!("Cannot send - mdns peer channel closed");
                                            }
                                        }
                                    } else {
                                        debug!(
                                            "Found mdns peer with unknown topic {}",
                                            their_topic
                                        );
                                    }
                                }
                                None => {
                                    warn!("Found mdns peer without topic property");
                                }
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
