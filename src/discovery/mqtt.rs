//! Peer discovery by publishing ip address (encrypted with topic name) to an MQTT server

use super::{hole_punch::HolePuncher, stun::NatType, topic::Topic, DiscoveredPeer, SessionToken};
use anyhow::anyhow;
use bincode::{deserialize, serialize};
use log::{debug, error, info, trace, warn};
use mqtt::{
    control::variable_header::ConnectReturnCode,
    packet::{
        ConnectPacket, PingreqPacket, PublishPacket, QoSWithPacketIdentifier, SubscribePacket,
        UnsubscribePacket, VariablePacket,
    },
    Encodable, QualityOfService, TopicFilter, TopicName,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::{SocketAddr, ToSocketAddrs},
    str,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    sync::{
        mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
};

// Keep alive timeout in seconds
const KEEP_ALIVE: u16 = 30;
const TCP_TIMEOUT: Duration = Duration::from_secs(120);

pub struct MqttClient {
    // topics: Arc<Mutex<HashMap<Topic, MqttTopic>>>,
    client_id: String,
    announce_address: AnnounceAddress,
    topic_events_tx: UnboundedSender<JoinOrLeaveEvent>,
}

impl MqttClient {
    pub async fn new(
        client_id: String,
        initial_topics: Vec<Topic>,
        public_addr: SocketAddr,
        nat_type: NatType,
        our_token: SessionToken,
        peers_tx: UnboundedSender<DiscoveredPeer>,
        hole_puncher: HolePuncher,
    ) -> anyhow::Result<Self> {
        let announce_address = AnnounceAddress {
            public_addr,
            nat_type,
            token: our_token,
        };

        let announce_address_clone = announce_address.clone();

        let (topic_events_tx, topic_events_rx) = unbounded_channel();

        let mqtt_client = Self {
            announce_address: announce_address_clone,
            client_id,
            topic_events_tx,
        };

        mqtt_client
            .run(peers_tx, hole_puncher, topic_events_rx)
            .await?;

        for topic in initial_topics {
            mqtt_client.add_topic(topic).await?;
        }

        Ok(mqtt_client)
    }

    pub async fn run(
        &self,
        peers_tx: UnboundedSender<DiscoveredPeer>,
        hole_puncher: HolePuncher,
        mut topic_events_rx: UnboundedReceiver<JoinOrLeaveEvent>,
    ) -> anyhow::Result<()> {
        let server_addr = "broker.hivemq.com:1883"
            .to_socket_addrs()?
            .find(|x| x.is_ipv4())
            .ok_or_else(|| anyhow!("Failed to get IP of MQTT server"))?;
        // TODO - An alternative: public.mqtthq.com:1883

        info!("Connecting to MQTT broker {:?} ... ", server_addr);
        let mut stream =
            tokio::time::timeout(TCP_TIMEOUT, TcpStream::connect(server_addr)).await??;

        info!("MQTT Client identifier {:?}", self.client_id);

        // Send a connect packet and check the response
        let mut connect_packet = ConnectPacket::new(self.client_id.clone());
        connect_packet.set_clean_session(true); // false?
        connect_packet.set_keep_alive(KEEP_ALIVE);
        let mut connect_packet_buf = Vec::new();
        connect_packet.encode(&mut connect_packet_buf)?;
        stream.write_all(&connect_packet_buf[..]).await?;

        let initial_response = VariablePacket::parse(&mut stream).await?;
        if let VariablePacket::ConnackPacket(connack) = initial_response {
            debug!("CONNACK {:?}", connack);

            if connack.connect_return_code() != ConnectReturnCode::ConnectionAccepted {
                return Err(anyhow!(
                    "Failed to connect to server, return code {:?}",
                    connack.connect_return_code()
                ));
            }
        } else {
            return Err(anyhow!("Got unexpected packet - expecting Connack"));
        }

        // Start a loop processing messages as a separate task
        let announce_address = self.announce_address.clone();
        tokio::spawn(async move {
            let mut topics = HashMap::<Topic, MqttTopic>::new();
            let mut subscribe_results = HashMap::<u16, (oneshot::Sender<bool>, Topic)>::new();
            let mut unsubscribe_results = HashMap::<u16, oneshot::Sender<bool>>::new();
            let mut packet_id_count = 10;
            let mut announcements_already_seen = HashSet::<Vec<u8>>::new();

            loop {
                tokio::select! {
                    Some(topic_event) = topic_events_rx.recv() => {
                        match topic_event {
                            JoinOrLeaveEvent::Join(topic, res_tx) => {
                                if let Ok(mqtt_topic) =
                                MqttTopic::new(&topic, true, true, &announce_address) {
                                    // Send a 'subscribe' message
                                    if let Ok(topic_filter) = TopicFilter::new(format!("hdp/{}", topic.public_id)) {
                                        let channel_filters = vec![(topic_filter, QualityOfService::Level0)];
                                        info!("Subscribing to channel {:?} ...", channel_filters);
                                        let sub = SubscribePacket::new(packet_id_count, channel_filters);
                                        let mut buf = Vec::new();
                                        if sub.encode(&mut buf).is_ok() && stream.write_all(&buf[..]).await.is_err() {
                                            warn!("Error writing subscribe packet");
                                            if res_tx.send(false).is_err() {
                                                error!("Channel closed");
                                            };
                                        } else {
                                            debug!("Subscribed, waiting for ack");
                                            subscribe_results.insert(packet_id_count, (res_tx, topic.clone()));
                                            packet_id_count += 1;
                                        };



                                        // Add the topic to our map so we keep sending publish
                                        // packets
                                        topics.insert(topic, mqtt_topic);
                                    } else {
                                        warn!("Could not create topic filter when subscribing to channel");
                                        if res_tx.send(false).is_err() {
                                            error!("Channel closed");
                                        };
                                    }
                                };
                            }
                            JoinOrLeaveEvent::Leave(topic, res_tx) => {
                                // Publish an 'unsubscribe' message
                                if let Ok(topic_filter) = TopicFilter::new(format!("hdp/{}", topic.public_id)) {
                                    let unsub = UnsubscribePacket::new(packet_id_count, vec![topic_filter]);
                                    let mut buf = Vec::new();
                                    if unsub.encode(&mut buf).is_ok() && stream.write_all(&buf[..]).await.is_err() {
                                        warn!("Error writing unsubscribe packet");
                                        if res_tx.send(false).is_err() {
                                            error!("Channel closed");
                                        };
                                    } else {
                                        unsubscribe_results.insert(packet_id_count, res_tx);
                                        packet_id_count += 1;
                                    };

                                } else {
                                    warn!("Could not create topic filter when unsubscribing to channel");
                                    if res_tx.send(false).is_err() {
                                        error!("Channel closed");
                                    };
                                };

                                // Remove the topic from our map so we stop announcing ourselves
                                topics.remove(&topic);
                            }
                        }
                    }
                    Ok(packet) = VariablePacket::parse(&mut stream) => {
                        trace!("PACKET {:?}", packet);

                        match packet {
                            VariablePacket::PingrespPacket(..) => {
                                trace!("Received PINGRESP from broker ..");
                            }
                            VariablePacket::PublishPacket(ref publ) => {
                                let payload = &publ.payload().to_vec();
                                if !announcements_already_seen.insert(payload.to_vec()) {
                                    debug!("Ignoring announce message already processed");
                                    continue;
                                }

                                // TODO handle err
                                if let Some((associated_topic, associated_mqtt_topic)) = topics.iter().find(|(topic, _mqtt_topic)| {
                                    let tn = &publ.topic_name();
                                    tn.contains(&topic.public_id)
                                }) {
                                    if let Some(remote_peer_announce) = decrypt_using_topic(&publ.payload().to_vec(), associated_topic) {
                                        if remote_peer_announce == announce_address {
                                            debug!("Found our own announce message");
                                            continue;
                                        }

                                        // Dont connect if we are both on the same IP - use mdns
                                        if remote_peer_announce.public_addr.ip() == announce_address.public_addr.ip() {
                                            debug!("Found remote peer with the same public ip as ours - ignoring");
                                            continue;
                                        }

                                        // TODO there are more cases when we should not bother hole punching
                                        if remote_peer_announce.nat_type != NatType::Symmetric {
                                            let mut hole_puncher_clone = hole_puncher.clone();
                                            tokio::spawn(async move {
                                                info!("Attempting hole punch...");
                                                if hole_puncher_clone.hole_punch_peer(remote_peer_announce.public_addr).await.is_err() {
                                                    warn!("Hole punching failed");
                                                } else {
                                                    info!("Hole punching succeeded");
                                                };
                                            });
                                        };

                                        // Decide whether to initiate the connection deterministically
                                        // so that only one party initiates
                                        let our_nat_badness = announce_address.nat_type as u8;
                                        let their_nat_badness = remote_peer_announce.nat_type as u8;
                                        let should_initiate_connection = if our_nat_badness == their_nat_badness {
                                                // If we both have the same NAT type, use the socket address
                                                // as a tie breaker
                                                let us = announce_address.public_addr.to_string();
                                                let them = remote_peer_announce.public_addr.to_string();
                                                us > them
                                        } else {
                                            // Otherwise the peer with the worst NAT type initiates the
                                            // connection
                                            our_nat_badness > their_nat_badness
                                        };

                                        if should_initiate_connection {
                                            info!("PUBLISH ({})", publ.topic_name());
                                            if peers_tx
                                                .send(DiscoveredPeer {
                                                    addr: remote_peer_announce.public_addr,
                                                    token: remote_peer_announce.token,
                                                    topic: Some(associated_topic.clone()),
                                                })
                                                .is_err()
                                            {
                                                error!("Cannot write to channel");
                                                break;
                                            }
                                        }

                                        // Say 'hello' by re-publishing our own announce message to
                                        // this topic
                                        if let Some(publish_packet) = &associated_mqtt_topic.publish_packet {
                                            if let Err(e) = stream.write_all(&publish_packet[..]).await {
                                                error!("Error when writing to mqtt broker {:?}", e);
                                                break;
                                            };
                                        }
                                    }
                                }
                            }
                            VariablePacket::SubackPacket(suback_packet) => {
                                debug!("Got suback packet");
                                match subscribe_results.remove(&suback_packet.packet_identifier()) {
                                    Some((res_tx, topic)) => {
                                        // TODO suback_packet.subscribes() != SubscribeReturnCode::Failure
                                        if res_tx.send(true).is_err() {
                                            error!("Cannot ackknowledge joining topic - channel closed");
                                        };

                                        if let Some(mqtt_topic) = topics.get(&topic) {
                                            // Publish our details to this topic
                                            if let Some(publish_packet) = &mqtt_topic.publish_packet {
                                                if let Err(e) = stream.write_all(&publish_packet[..]).await {
                                                    error!("Error when writing to mqtt broker {:?}", e);
                                                    break;
                                                };
                                            }
                                        }
                                    }
                                    None => {
                                        warn!("Got unexpected suback message");
                                    }
                                };
                            },
                            VariablePacket::UnsubackPacket(unsuback_packet) => {
                                debug!("Got unsuback packet");
                                match unsubscribe_results.remove(&unsuback_packet.packet_identifier()) {
                                    Some(res_tx) => {
                                        // TODO suback_packet.subscribes() != SubscribeReturnCode::Failure
                                        if res_tx.send(true).is_err() {
                                            error!("Cannot ackknowledge joining topic - channel closed");
                                        };
                                    }
                                    None => {
                                        warn!("Got unexpected suback message");
                                    }
                                };
                            },
                            _ => {}
                        }
                    }
                    // Send ping packets to keep the connection open
                    () = tokio::time::sleep(Duration::from_secs(KEEP_ALIVE as u64 / 2)) => {
                        trace!("Sending PINGREQ to broker");

                        let pingreq_packet = PingreqPacket::new();

                        let mut pingreq_packet_buf = Vec::new();
                        if pingreq_packet.encode(&mut pingreq_packet_buf).is_err() {
                            error!("Cannot encode MQTT ping packet");
                            break;
                        };
                        if stream.write_all(&pingreq_packet_buf).await.is_err() {
                            error!("Cannot write MQTT ping packet");
                            // break;
                        };
                    }
                }
            }
        });
        Ok(())
    }

    pub async fn add_topic(&self, topic: Topic) -> anyhow::Result<()> {
        // TODO this could contain a oneshot with a result showing if subscribing was successful
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
        // TODO this could contain a oneshot with a result showing if unsubscribing was successful
        let (tx, rx) = oneshot::channel();
        self.topic_events_tx
            .send(JoinOrLeaveEvent::Leave(topic, tx))?;

        if let Ok(true) = rx.await {
            Ok(())
        } else {
            Err(anyhow!("Failed to add topic"))
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct AnnounceAddress {
    public_addr: SocketAddr,
    nat_type: NatType,
    token: SessionToken,
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

#[derive(Debug)]
pub enum JoinOrLeaveEvent {
    Join(Topic, oneshot::Sender<bool>),
    Leave(Topic, oneshot::Sender<bool>),
}

#[derive(Hash, Eq, PartialEq)]
struct MqttTopic {
    // topic: Topic,
    publish_packet: Option<Vec<u8>>,
    subscribe: bool,
}

impl MqttTopic {
    fn new(
        topic: &Topic,
        publish: bool,
        subscribe: bool,
        announce_address: &AnnounceAddress,
    ) -> anyhow::Result<Self> {
        let publish_packet = if publish {
            Some(create_publish_packet(topic, announce_address)?)
        } else {
            None
        };

        Ok(Self {
            // topic,
            publish_packet,
            subscribe,
        })
    }
}

fn create_publish_packet(
    topic: &Topic,
    announce_address: &AnnounceAddress,
) -> anyhow::Result<Vec<u8>> {
    let announce_cleartext = serialize(&announce_address)?;

    // TODO do we need a unique topic for each peer?
    let channel_name = TopicName::new(format!("hdp/{}", topic.public_id))?;

    let encrypted_announce = topic.encrypt(&announce_cleartext);
    let mut publish_packet = PublishPacket::new(
        channel_name,
        QoSWithPacketIdentifier::Level0,
        encrypted_announce,
    );
    publish_packet.set_retain(false);
    let mut publish_packet_buf = Vec::new();
    publish_packet.encode(&mut publish_packet_buf)?;
    Ok(publish_packet_buf)
}
