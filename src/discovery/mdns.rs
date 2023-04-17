use crate::discovery::{
    capability::{handshake_request, handshake_response, HandshakeRequest},
    topic::Topic,
    DiscoveredPeer, SessionToken,
};
use anyhow::anyhow;
use log::{debug, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::{IpAddr, SocketAddr};
use tokio::sync::mpsc::UnboundedSender;

const SERVICE_TYPE: &str = "_hdp._udp.local.";
const TOPIC: &str = "topic";

// pub struct MdnsServer {}
//
// impl MdnsServer {
//     pub fn new() {}
//
//     pub fn add_topic(topic: Topic) {}
//
//     pub fn remove_topic(topic: Topic) {}
// }

// TODO i dont think we need the port proptery
// TODO allow multiple topics
/// Announce ourself on mdns and discover other local peers
pub async fn mdns_server(
    id: &str,
    addr: SocketAddr,
    initial_topics: Vec<Topic>,
    peers_tx: UnboundedSender<DiscoveredPeer>,
    token: SessionToken,
) -> anyhow::Result<()> {
    let mdns = ServiceDaemon::new()?;

    let capabilities: Vec<HandshakeRequest> = initial_topics
        .iter()
        .map(|topic| handshake_request(&topic, addr, token))
        .collect();

    // Create a service info.
    let host_name = "localhost"; // TODO
    let mut properties = std::collections::HashMap::new();

    let mut topic_count = 0;
    for capability in capabilities {
        properties.insert(
            format!("{}{}", TOPIC.to_string(), topic_count),
            hex::encode(capability),
        );
        topic_count += 1;
    }

    if let IpAddr::V4(ipv4_addr) = addr.ip() {
        let my_service = ServiceInfo::new(
            SERVICE_TYPE,
            &id[0..16],
            host_name,
            ipv4_addr,
            addr.port(), //+ 150, // TODO
            Some(properties),
        )?;
        // .enable_addr_auto();

        // Register with the daemon, which publishes the service.
        mdns.register(my_service)?;

        let mdns_receiver = mdns.browse(SERVICE_TYPE)?;

        tokio::spawn(async move {
            // Receive the browse events in sync or async. Here is
            // an example of using a thread. Users can call `receiver.recv_async().await`
            // if running in async environment.
            while let Ok(event) = mdns_receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        debug!("Resolved a mdns service: {:?}", info);
                        match parse_peer_info(info) {
                            Ok((their_addr, capabilities)) => {
                                if their_addr == addr {
                                    debug!("Found ourself on mdns");
                                } else if let Some(their_token) =
                                    try_topics(&initial_topics, capabilities, their_addr)
                                {
                                    // Only connect if our address is lexicographicaly greater than
                                    // theirs - to prevent duplicate connections
                                    let us = addr.to_string();
                                    let them = their_addr.to_string();
                                    if us > them
                                        && peers_tx
                                            .send(DiscoveredPeer {
                                                addr: their_addr,
                                                token: their_token,
                                                topic: None,
                                            })
                                            .is_err()
                                    {
                                        warn!("Cannot send - peer discovery channel closed");
                                    }
                                } else {
                                    warn!("Found mdns peer with unknown/bad capability");
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
    } else {
        Err(anyhow!("ipv6 address cannot be used for MDNS"))
    }
}

fn parse_peer_info(info: ServiceInfo) -> anyhow::Result<(SocketAddr, Vec<HandshakeRequest>)> {
    if info.get_type() != SERVICE_TYPE {
        return Err(anyhow!("Peer does not have expected service type"));
    }

    let properties = info.get_properties();

    let capabilities = properties
        .values()
        .filter_map(|capability_string| {
            if let Ok(buf) = hex::decode(capability_string) {
                buf.try_into().ok()
            } else {
                None
            }
        })
        .collect();

    let their_ip = info
        .get_addresses()
        .iter()
        .next()
        .ok_or_else(|| anyhow!("Cannot get ip"))?;

    let their_port = info.get_port();

    let addr = SocketAddr::new(IpAddr::V4(*their_ip), their_port);
    Ok((addr, capabilities))
}

// Find a capability which matches one of our connected topics
fn try_topics(
    topics: &Vec<Topic>,
    capabilities: Vec<HandshakeRequest>,
    their_addr: SocketAddr,
) -> Option<SessionToken> {
    for capability in capabilities {
        for topic in topics {
            if let Ok(their_token) = handshake_response(capability, &topic, their_addr) {
                return Some(their_token);
            }
        }
    }
    None
}
