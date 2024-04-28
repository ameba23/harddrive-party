//! Peer discovery on local network using mDNS
use crate::discovery::{
    capability::{handshake_request, handshake_response, HandshakeRequest},
    topic::Topic,
    DiscoveredPeer, JoinOrLeaveEvent, SessionToken,
};
use anyhow::anyhow;
use log::{debug, error, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo, UnregisterStatus};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    oneshot,
};

const SERVICE_TYPE: &str = "_hdp._udp.local.";
const TOPIC: &str = "topic";

/// Announces ourself on mDNS
pub struct MdnsServer {
    /// Notifies us when joining or leaving a topic
    topic_events_tx: UnboundedSender<JoinOrLeaveEvent>,
}

impl MdnsServer {
    pub async fn new(
        id: &str,
        addr: SocketAddr,
        peers_tx: UnboundedSender<DiscoveredPeer>,
        token: SessionToken,
        initial_topics: HashSet<Topic>,
    ) -> anyhow::Result<Self> {
        let (topic_events_tx, topic_events_rx) = unbounded_channel();
        let mdns_server = Self { topic_events_tx };

        mdns_server.run(id, addr, token, peers_tx, topic_events_rx, initial_topics)?;
        Ok(mdns_server)
    }

    fn run(
        &self,
        id: &str,
        addr: SocketAddr,
        token: SessionToken,
        peers_tx: UnboundedSender<DiscoveredPeer>,
        mut topic_events_rx: UnboundedReceiver<JoinOrLeaveEvent>,
        initial_topics: HashSet<Topic>,
    ) -> anyhow::Result<()> {
        let mdns = ServiceDaemon::new()?;
        let mdns_receiver = mdns.browse(SERVICE_TYPE)?;

        let id_clone = id.to_string();
        tokio::spawn(async move {
            let mut topics = initial_topics; //HashSet<Topic> = Default::default();
            let mut existing_service: Option<String> = None;

            loop {
                tokio::select! {
                    Some(topic_event) = topic_events_rx.recv() => {
                        // Get the oneshot which we use to confirm that joining or leaving was
                        // succesful.
                        let res_tx = match topic_event {
                            JoinOrLeaveEvent::Join(topic, res_tx) => {
                                topics.insert(topic);
                                res_tx
                            }
                            JoinOrLeaveEvent::Leave(topic, res_tx) => {
                                topics.remove(&topic);
                                res_tx
                            }
                        };

                        if let Ok(service) = create_service_info(&topics, &id_clone, &addr, &token) {
                            if let Some(existing_service_name) = existing_service {
                                if let Ok(receiver) = mdns.unregister(&existing_service_name) {
                                    debug!("Unregistering service");
                                    let unregister_status = receiver.recv_async().await;
                                    match unregister_status {
                                        Ok(UnregisterStatus::OK) => {
                                            debug!("Unregister succesful");
                                        }
                                        Ok(UnregisterStatus::NotFound) => {
                                            warn!("Tried to unregister mDNS service, but it was not found");
                                        }
                                        Err(e) => {
                                            error!("{:?}", e);
                                        }
                                    }
                                } else {
                                    warn!("Cannot unregister service");
                                };
                            };

                            existing_service = Some(service.get_fullname().to_string().clone());
                            if mdns.register(service).is_ok() {
                                debug!("Registered service");
                                if res_tx.send(true).is_err() {
                                    error!("Cannot acknowledge registering mdns service - channel closed");
                                };
                            } else {
                                error!("Failed to register service");
                                if res_tx.send(false).is_err() {
                                    error!("Cannot acknowledge registering mdns service - channel closed");
                                };
                            };
                        } else {
                            warn!("Cannot create service");
                            if res_tx.send(false).is_err() {
                                error!("Cannot acknowledge registering mdns service - channel closed");
                            };
                        }
                    }
                    Ok(event) = mdns_receiver.recv_async() => {
                        match event {
                            ServiceEvent::ServiceResolved(info) => {
                                debug!("Resolved a mdns service: {:?}", info);
                                match parse_peer_info(info) {
                                    Ok((their_addr, capabilities)) => {
                                        if their_addr == addr {
                                            debug!("Found ourself on mdns");
                                        } else if let Some(their_token) =
                                        try_topics(&topics, capabilities, their_addr)
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
                }
            }
        });

        Ok(())
    }

    pub async fn add_topic(&self, topic: Topic) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.topic_events_tx
            .send(JoinOrLeaveEvent::Join(topic, tx))?;
        if let Ok(true) = rx.await {
            Ok(())
        } else {
            Err(anyhow!("Failed to add topic"))
        }
    }

    pub async fn remove_topic(&self, topic: Topic) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.topic_events_tx
            .send(JoinOrLeaveEvent::Leave(topic, tx))?;

        if let Ok(true) = rx.await {
            Ok(())
        } else {
            Err(anyhow!("Failed to remove topic"))
        }
    }
}

fn create_service_info(
    topics: &HashSet<Topic>,
    id: &str,
    addr: &SocketAddr,
    token: &SessionToken,
) -> anyhow::Result<ServiceInfo> {
    let capabilities: Vec<HandshakeRequest> = topics
        .iter()
        .map(|topic| handshake_request(topic, addr, token))
        .collect();

    // Create a service info.
    let host_name = "localhost"; // TODO
    let mut properties = std::collections::HashMap::new();

    for (topic_count, capability) in capabilities.into_iter().enumerate() {
        properties.insert(format!("{}{}", TOPIC, topic_count), hex::encode(capability));
    }

    if let IpAddr::V4(ipv4_addr) = addr.ip() {
        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            &id[0..16],
            host_name,
            ipv4_addr,
            addr.port(), //+ 150, // TODO
            Some(properties),
        )?;
        Ok(service_info)
    } else {
        Err(anyhow!("ipv6 address cannot be used for MDNS"))
    }
}

// TODO i dont think we need the port proptery
// TODO allow multiple topics
/// Announce ourself on mdns and discover other local peers
// pub async fn mdns_server(
//     id: &str,
//     addr: SocketAddr,
//     initial_topics: Vec<Topic>,
//     peers_tx: UnboundedSender<DiscoveredPeer>,
//     token: SessionToken,
// ) -> anyhow::Result<()> {
//     let mdns = ServiceDaemon::new()?;
//
//     let capabilities: Vec<HandshakeRequest> = initial_topics
//         .iter()
//         .map(|topic| handshake_request(&topic, addr, token))
//         .collect();
//
//     // Create a service info.
//     let host_name = "localhost"; // TODO
//     let mut properties = std::collections::HashMap::new();
//
//     let mut topic_count = 0;
//     for capability in capabilities {
//         properties.insert(
//             format!("{}{}", TOPIC.to_string(), topic_count),
//             hex::encode(capability),
//         );
//         topic_count += 1;
//     }
//
//     if let IpAddr::V4(ipv4_addr) = addr.ip() {
//         let my_service = ServiceInfo::new(
//             SERVICE_TYPE,
//             &id[0..16],
//             host_name,
//             ipv4_addr,
//             addr.port(), //+ 150, // TODO
//             Some(properties),
//         )?;
//         // .enable_addr_auto();
//
//         // Register with the daemon, which publishes the service.
//         mdns.register(my_service)?;
//
//         let mdns_receiver = mdns.browse(SERVICE_TYPE)?;
//
//         tokio::spawn(async move {
//             // Receive the browse events in sync or async. Here is
//             // an example of using a thread. Users can call `receiver.recv_async().await`
//             // if running in async environment.
//             while let Ok(event) = mdns_receiver.recv() {
//                 match event {
//                     ServiceEvent::ServiceResolved(info) => {
//                         debug!("Resolved a mdns service: {:?}", info);
//                         match parse_peer_info(info) {
//                             Ok((their_addr, capabilities)) => {
//                                 if their_addr == addr {
//                                     debug!("Found ourself on mdns");
//                                 } else if let Some(their_token) =
//                                     try_topics(&initial_topics, capabilities, their_addr)
//                                 {
//                                     // Only connect if our address is lexicographicaly greater than
//                                     // theirs - to prevent duplicate connections
//                                     let us = addr.to_string();
//                                     let them = their_addr.to_string();
//                                     if us > them
//                                         && peers_tx
//                                             .send(DiscoveredPeer {
//                                                 addr: their_addr,
//                                                 token: their_token,
//                                                 topic: None,
//                                             })
//                                             .is_err()
//                                     {
//                                         warn!("Cannot send - peer discovery channel closed");
//                                     }
//                                 } else {
//                                     warn!("Found mdns peer with unknown/bad capability");
//                                 }
//                             }
//                             Err(error) => {
//                                 warn!("Invalid mdns peer found {:?}", error);
//                             }
//                         }
//                     }
//                     ServiceEvent::ServiceRemoved(_type, fullname) => {
//                         debug!("mdns peer removed {:?}", &fullname);
//                     }
//                     _ => {}
//                 }
//             }
//         });
//         Ok(())
//     } else {
//         Err(anyhow!("ipv6 address cannot be used for MDNS"))
//     }
// }

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
                warn!("Cannot decode hex in mdns property");
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
// TODO return also the associated topic
fn try_topics(
    topics: &HashSet<Topic>,
    capabilities: Vec<HandshakeRequest>,
    their_addr: SocketAddr,
) -> Option<SessionToken> {
    for capability in capabilities {
        println!("Tring a capability");
        for topic in topics {
            println!("checking a topic");
            if let Ok(their_token) = handshake_response(capability, topic, their_addr) {
                return Some(their_token);
            }
        }
    }
    None
}
