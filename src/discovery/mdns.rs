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
// const PORT: &str = "port";

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
    topic: Topic,
    peers_tx: UnboundedSender<DiscoveredPeer>,
    token: SessionToken,
) -> anyhow::Result<()> {
    let mdns = ServiceDaemon::new()?;

    let capability = handshake_request(&topic, addr, token);

    // Create a service info.
    let host_name = "localhost"; // TODO
    let mut properties = std::collections::HashMap::new();
    properties.insert(TOPIC.to_string(), hex::encode(capability));
    // properties.insert(PORT.to_string(), addr.port().to_string());

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
                            Ok((their_addr, capability)) => {
                                if their_addr == addr {
                                    debug!("Found ourself on mdns");
                                } else if let Ok(their_token) =
                                    handshake_response(capability, &topic, their_addr)
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

fn parse_peer_info(info: ServiceInfo) -> anyhow::Result<(SocketAddr, HandshakeRequest)> {
    if info.get_type() != SERVICE_TYPE {
        return Err(anyhow!("Peer does not have expected service type"));
    }

    let properties = info.get_properties();

    let capability = hex::decode(
        properties
            .get(&TOPIC.to_string())
            .ok_or_else(|| anyhow!("Cannot get topic"))?,
    )?
    .try_into()
    .map_err(|_| anyhow!("Cannot decode hex"))?;

    let their_ip = info
        .get_addresses()
        .iter()
        .next()
        .ok_or_else(|| anyhow!("Cannot get ip"))?;

    let their_port = info.get_port();
    // let their_port = properties
    //     .get(&PORT.to_string())
    //     .ok_or_else(|| anyhow!("Cannot get port"))?
    //     .parse::<u16>()?;

    let addr = SocketAddr::new(IpAddr::V4(*their_ip), their_port);
    Ok((addr, capability))
}
