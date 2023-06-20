use super::{
    hole_punch::HolePuncher, should_connect_to_peer, topic::Topic, AnnounceAddress, DiscoveredPeer,
    JoinOrLeaveEvent,
};
use anyhow::anyhow;
use bincode::{deserialize, serialize};
use libp2p::gossipsub::IdentTopic;
use libp2p::swarm::SwarmBuilder;
use libp2p::{
    dns, futures::StreamExt, gossipsub::Behaviour, gossipsub::Event, identity::Keypair,
    swarm::SwarmEvent, tcp, Multiaddr, PeerId,
};
use libp2p::{gossipsub, Transport};
use log::{error, info};
use protobuf::Message;
use std::{
    collections::hash_map::DefaultHasher,
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    // net::{SocketAddr, ToSocketAddrs},
    str,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

include!(concat!(env!("OUT_DIR"), "/protos/mod.rs"));
use waku_message::WakuMessage;

// const CONTENT_TOPIC: &str = "/hdp/1/{}/proto";
const RELAY_PROTOCOL_ID: &str = "/vac/waku/relay/2.0.0";
const DEFAULT_PUBSUB_TOPIC: &str = "/waku/2/default-waku/proto";

// Waku bootstrap nodes
const NODES: &[&str] = &[
    "/dns4/node-01.ac-cn-hongkong-c.wakuv2.test.statusim.net/tcp/30303/p2p/16Uiu2HAkvWiyFsgRhuJEb9JfjYxEkoHLgnUQmr1N5mKWnYjxYRVm",
    "/dns4/node-01.do-ams3.wakuv2.test.statusim.net/tcp/30303/p2p/16Uiu2HAmPLe7Mzm8TsYUubgCAW1aJoeFScxrLj8ppHFivPo97bUZ",
    "/dns4/node-01.gc-us-central1-a.wakuv2.test.statusim.net/tcp/30303/p2p/16Uiu2HAmJb2e28qLXxT5kZxVUUoJt72EMzNGXB47Rxx5hw3q4YjS"
];

pub struct WakuDiscovery {
    announce_address: AnnounceAddress,
    topic_events_tx: UnboundedSender<JoinOrLeaveEvent>,
}

impl WakuDiscovery {
    pub async fn new(
        announce_address: AnnounceAddress,
        peers_tx: UnboundedSender<DiscoveredPeer>,
        hole_puncher: HolePuncher,
    ) -> anyhow::Result<Self> {
        // let announce_address_clone = announce_address.clone();

        let (topic_events_tx, topic_events_rx) = unbounded_channel();

        let waku_discovery = Self {
            announce_address,
            topic_events_tx,
        };

        waku_discovery
            .run(peers_tx, hole_puncher, topic_events_rx)
            .await?;

        Ok(waku_discovery)
    }

    pub async fn run(
        &self,
        peers_tx: UnboundedSender<DiscoveredPeer>,
        mut hole_puncher: HolePuncher,
        mut topic_events_rx: UnboundedReceiver<JoinOrLeaveEvent>,
    ) -> anyhow::Result<()> {
        let local_key = Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(local_key.public());
        info!("Libp2p local peer id: {:?}", local_peer_id);

        let transport = create_transport(local_key)?;

        let message_id_fn = |message: &gossipsub::Message| {
            let mut s = DefaultHasher::new();
            message.data.hash(&mut s);
            gossipsub::MessageId::from(s.finish().to_string())
        };

        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .protocol_id(RELAY_PROTOCOL_ID, gossipsub::Version::V1_1)
            .validation_mode(gossipsub::ValidationMode::Anonymous) // StrictNoSign
            .message_id_fn(message_id_fn)
            .build()
            .map_err(|e| anyhow!(e))?;

        let mut behaviour: Behaviour = Behaviour::new(
            libp2p::gossipsub::MessageAuthenticity::Anonymous,
            gossipsub_config,
        )
        .map_err(|e| anyhow!(e))?;

        behaviour.subscribe(&IdentTopic::new(DEFAULT_PUBSUB_TOPIC))?;

        let mut swarm =
            SwarmBuilder::with_tokio_executor(transport, behaviour, local_peer_id).build();
        swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

        // Connect to all default nodes
        for node in NODES {
            let address: Multiaddr = node.parse()?;
            match swarm.dial(address.clone()) {
                Ok(_) => info!("Dialed {:?}", address),
                Err(e) => error!("failed to dial address: {:?} {:?}", address, e),
            }
        }

        let mut already_seen_announcements = HashSet::new();
        let our_announce_address = self.announce_address.clone();
        tokio::spawn(async move {
            let mut topics = HashMap::<String, AnnouncePayload>::new();
            loop {
                tokio::select! {
                    event = swarm.select_next_some() => {
                        match event {
                            SwarmEvent::Behaviour(Event::Subscribed { peer_id: _, topic }) => {
                                info!(" Suscribed to {:?}", topic);
                                if topic.as_str() == DEFAULT_PUBSUB_TOPIC {
                                    // When we have subscibed, we send our topic messages
                                    for announce in topics.values() {
                                        if let Ok(announce_encoded) = announce.encode_with_timestamp() {
                                            match swarm.behaviour_mut().publish(
                                                IdentTopic::new(DEFAULT_PUBSUB_TOPIC),
                                                announce_encoded,
                                            ) {
                                                Ok(m) => info!("Published message: {}", m),
                                                Err(e) => error!("Error publishing message: {}", e),
                                            };
                                        } else {
                                            error!("Cannot encode announce message");
                                            break;
                                        }
                                    }
                                }
                            }
                            SwarmEvent::Behaviour(Event::Message {
                                propagation_source: _,
                                message_id: _,
                                message,
                            }) => {
                                let topic = message.topic;
                                if let Ok(waku_message) = WakuMessage::parse_from_bytes(&message.data) {
                                    info!("Topic: {} Msg: {:?}", topic, waku_message);
                                    if already_seen_announcements.insert(waku_message.payload.clone()) {
                                        // Check if the content_topic matches one of ours
                                        if let Some(announce) = topics.get(&waku_message.content_topic) {
                                            if let Some(remote_peer_announce) = decrypt_using_topic(&waku_message.payload, &announce.topic) {
                                                // Logic as to whether we want to connect to this peer
                                                let (should_connect, should_holepunch) = should_connect_to_peer(&remote_peer_announce, &our_announce_address);

                                                if should_holepunch {
                                                    hole_puncher.spawn_hole_punch_peer(remote_peer_announce.public_addr);
                                                }

                                                if should_connect && peers_tx
                                                    .send(DiscoveredPeer {
                                                        addr: remote_peer_announce.public_addr,
                                                        token: remote_peer_announce.token,
                                                        topic: Some(announce.topic.clone()),
                                                    })
                                                .is_err()
                                                {
                                                    error!("Cannot write to channel");
                                                    break;
                                                }

                                                // Say 'hello' be announcing ourselves back on this topic
                                                if let Ok(announce_encoded) = announce.encode_with_timestamp() {
                                                    match swarm.behaviour_mut().publish(
                                                        IdentTopic::new(DEFAULT_PUBSUB_TOPIC),
                                                        announce_encoded,
                                                    ) {
                                                        Ok(m) => info!("Published message: {}", m),
                                                        Err(e) => error!("Error publishing message: {}", e),
                                                    };
                                                } else {
                                                    error!("Cannot encode announce message");
                                                    break;
                                                }
                                            }
                                        }
                                    };
                                } else {
                                    error!("Could not parse waku message");
                                };
                            }
                            SwarmEvent::Behaviour(Event::GossipsubNotSupported { peer_id: _ }) => {
                                error!("GossipsubNotSupported");
                                break;
                            }
                            _ => {
                                info!("{:?}", event);
                            }
                        }
                    }
                    Some(topic_event) = topic_events_rx.recv() => {
                        match topic_event {
                            JoinOrLeaveEvent::Join(topic, res_tx) => {
                                let topic_name =
                                        format!("/hdp/1/{}/proto", topic.public_id);
                                let success = if let Ok(announce) = AnnouncePayload::new(topic, &our_announce_address) {
                                    topics.insert(topic_name, announce);
                                    true
                                } else {
                                    false
                                };
                                if res_tx.send(success).is_err() {
                                    error!("Channel closed");
                                };
                            }
                            JoinOrLeaveEvent::Leave(topic, res_tx) => {
                                // Remove the topic from our map so we stop announcing ourselves
                                let topic_name =
                                        format!("/hdp/1/{}/proto", topic.public_id);
                                topics.remove(&topic_name);
                                if res_tx.send(true).is_err() {
                                    error!("Channel closed");
                                };
                            }
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

pub fn create_transport(
    keypair: Keypair,
) -> anyhow::Result<libp2p::core::transport::Boxed<(PeerId, libp2p::core::muxing::StreamMuxerBox)>>
{
    let transport =
        dns::TokioDnsConfig::system(tcp::tokio::Transport::new(tcp::Config::new().nodelay(true)))?;

    Ok(transport
        .upgrade(libp2p::core::upgrade::Version::V1)
        .authenticate(libp2p::noise::Config::new(&keypair)?)
        .multiplex(libp2p::core::upgrade::SelectUpgrade::new(
            libp2p::yamux::Config::default(),
            #[allow(deprecated)]
            libp2p::mplex::MplexConfig::default(),
        ))
        .timeout(std::time::Duration::from_secs(20))
        .boxed())
}

struct AnnouncePayload {
    topic: Topic,
    payload: Vec<u8>,
}

impl AnnouncePayload {
    fn new(topic: Topic, announce_address: &AnnounceAddress) -> anyhow::Result<Self> {
        let announce_cleartext = serialize(&announce_address)?;
        let payload = topic.encrypt(&announce_cleartext);
        Ok(Self { payload, topic })
    }

    fn encode_with_timestamp(&self) -> anyhow::Result<Vec<u8>> {
        let mut msg = WakuMessage::new();
        msg.payload = self.payload.clone();
        msg.content_topic = format!("/hdp/1/{}/proto", self.topic.public_id);
        msg.timestamp = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_secs()
                .try_into()?,
        );
        Ok(msg.write_to_bytes()?)
    }
}

// Attempt to decrypt an announce message from another peer
fn decrypt_using_topic(payload: &Vec<u8>, topic: &Topic) -> Option<AnnounceAddress> {
    if let Some(announce_message_bytes) = topic.decrypt(payload) {
        let announce_address_result: Result<AnnounceAddress, Box<bincode::ErrorKind>> =
            deserialize(&announce_message_bytes);
        if let Ok(announce_address) = announce_address_result {
            return Some(announce_address);
        }
    }
    None
}
