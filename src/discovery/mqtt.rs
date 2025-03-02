//! Peer discovery by publishing ip address (encrypted with topic name) to an MQTT server
use super::{
    handle_peer, hole_punch::HolePuncher, topic::Topic, AnnounceAddress, DiscoveredPeer,
    JoinOrLeaveEvent,
};
use anyhow::anyhow;
use bincode::{deserialize, serialize};
use log::{debug, error, info, trace, warn};
use mqtt::{
    control::variable_header::ConnectReturnCode,
    packet::{
        suback::SubscribeReturnCode, ConnectPacket, PingreqPacket, PublishPacket,
        QoSWithPacketIdentifier, SubscribePacket, UnsubscribePacket, VariablePacket,
    },
    Encodable, QualityOfService, TopicFilter, TopicName,
};
use std::{
    collections::{HashMap, HashSet},
    net::{SocketAddr, ToSocketAddrs},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    sync::{
        mpsc::{channel, Receiver, Sender},
        oneshot,
    },
};

// Keep alive timeout in seconds
const KEEP_ALIVE: u16 = 30;
const TCP_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_MQTT_SERVER: &str = "broker.hivemq.com:1883";

pub struct MqttClient {
    // topics: Arc<Mutex<HashMap<Topic, MqttTopic>>>,
    client_id: String,
    announce_address: AnnounceAddress,
    topic_events_tx: Sender<JoinOrLeaveEvent>,
    mqtt_server: String,
}

impl MqttClient {
    pub async fn new(
        client_id: String,
        announce_address: AnnounceAddress,
        peers_tx: Sender<DiscoveredPeer>,
        hole_puncher: Option<HolePuncher>,
        mqtt_server: Option<String>,
    ) -> anyhow::Result<Self> {
        let announce_address_clone = announce_address.clone();

        let (topic_events_tx, topic_events_rx) = channel(1024);

        let mqtt_client = Self {
            announce_address: announce_address_clone,
            client_id,
            topic_events_tx,
            mqtt_server: mqtt_server.unwrap_or_else(|| DEFAULT_MQTT_SERVER.to_string()),
        };

        mqtt_client
            .run(peers_tx, hole_puncher, topic_events_rx)
            .await?;

        Ok(mqtt_client)
    }

    pub async fn run(
        &self,
        peers_tx: Sender<DiscoveredPeer>,
        hole_puncher: Option<HolePuncher>,
        mut topic_events_rx: Receiver<JoinOrLeaveEvent>,
    ) -> anyhow::Result<()> {
        let mqtt_server = self.mqtt_server.clone();
        let mut server_addr = mqtt_server
            .to_socket_addrs()?
            .find(|x| x.is_ipv4())
            .ok_or_else(|| anyhow!("Failed to get IP of MQTT server"))?;
        // TODO - An alternative: public.mqtthq.com:1883

        info!("MQTT Client identifier {:?}", self.client_id);

        let mut stream = connect(&server_addr, self.client_id.clone()).await?;

        // Start a loop processing messages as a separate task
        let announce_address = self.announce_address.clone();
        let client_id = self.client_id.clone();
        tokio::spawn(async move {
            let mut topics = HashMap::<Topic, MqttTopic>::new();

            // Loop of reconnections following connection lost
            loop {
                // A map of packet ids to oneshots for pending subscribe requests, together with
                // the associated topic
                let mut subscribe_results = HashMap::<u16, (oneshot::Sender<bool>, Topic)>::new();
                // A map of packet ids to oneshots for pending unsubscribe requests
                let mut unsubscribe_results = HashMap::<u16, oneshot::Sender<bool>>::new();
                // Start the packet id counter at 10 to account for initial connect messages
                let mut packet_id_count = 10;
                // Announce messages we don't need to act on because we already did
                let mut announcements_already_seen = HashSet::<Vec<u8>>::new();

                let reconnect = loop {
                    tokio::select! {
                        Some(topic_event) = topic_events_rx.recv() => {
                            match topic_event {
                                JoinOrLeaveEvent::Join(topic, res_tx) => {
                                    if let Ok(mqtt_topic) =
                                    MqttTopic::new(&topic, &announce_address) {
                                        // Send a 'subscribe' message
                                        let channel_filters = vec![(mqtt_topic.topic_filter.clone(), QualityOfService::Level0)];
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
                                        if let Some(remote_peer_announce) = decrypt_using_topic(publ.payload(), associated_topic) {
                                            if remote_peer_announce == announce_address {
                                                debug!("Found our own announce message");
                                                continue;
                                            }

                                            // Dont connect if we are both on the same IP - use mdns
                                            if remote_peer_announce.connection_details.ip() == announce_address.connection_details.ip() {
                                                debug!("Found remote peer with the same public ip as ours - ignoring");
                                                continue;
                                            }

                                            // Say 'hello' by re-publishing our own announce message to
                                            // this topic
                                            if let Err(e) = stream.write_all(&associated_mqtt_topic.publish_packet[..]).await {
                                                error!("Error when writing to mqtt broker {:?}", e);
                                                break true;
                                            };
                                            debug!("Remote peer {:?}", remote_peer_announce);
                                            let hole_puncher_clone = hole_puncher.clone();
                                            let connection_details = announce_address.connection_details.clone();
                                            let peers_tx = peers_tx.clone();
                                            tokio::spawn(async move {
                                                match handle_peer(hole_puncher_clone, connection_details, remote_peer_announce).await {
                                                    Ok(Some(discovered_peer)) => {
                                                        debug!("Connect to {:?}", discovered_peer);
                                                        if peers_tx
                                                            .send(discovered_peer).await
                                                                .is_err()
                                                        {
                                                            error!("Cannot write to channel");
                                                        }
                                                    }
                                                    Ok(None) => {
                                                        debug!("Successfully handled peer - awaiting connection from their side");
                                                    }
                                                    Err(error) => {
                                                        warn!("Error when handling discovered peer {:?}", error);
                                                    }
                                                }
                                            });
                                        }
                                    }
                                }
                                VariablePacket::SubackPacket(suback_packet) => {
                                    debug!("Got suback packet");
                                    match subscribe_results.remove(&suback_packet.packet_identifier()) {
                                        Some((res_tx, topic)) => {
                                            let success = suback_packet.subscribes() != [SubscribeReturnCode::Failure];
                                            if res_tx.send(success).is_err() {
                                                error!("Cannot ackknowledge joining topic - channel closed");
                                            };
                                            if success {
                                                if let Some(mqtt_topic) = topics.get(&topic) {
                                                    // Publish our details to this topic
                                                    if let Err(e) = stream.write_all(&mqtt_topic.publish_packet[..]).await {
                                                        error!("Error when writing to mqtt broker {:?}", e);
                                                        break true;
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
                                break false;
                            };
                            if stream.write_all(&pingreq_packet_buf).await.is_err() {
                                error!("Cannot write MQTT ping packet");
                                break true;
                            };
                        }
                    }
                };
                if reconnect {
                    if stream.shutdown().await.is_err() {
                        error!("Error while shutting down stream");
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let reconnect_success = loop {
                        if let Ok(new_stream) = connect(&server_addr, client_id.clone()).await {
                            stream = new_stream;
                            break true;
                        } else {
                            error!("Cannot connect to mqtt server - reconnecting in 10s");
                            tokio::time::sleep(Duration::from_secs(10)).await;

                            // Do DNS lookup again
                            match mqtt_dns_resolve(&mqtt_server) {
                                Ok(addr) => {
                                    server_addr = addr;
                                }
                                Err(err) => {
                                    warn!("{:?}", err);
                                    break false;
                                }
                            }
                        }
                    };
                    if !reconnect_success {
                        break;
                    }
                    // Resubscribe to existing topics
                    let channel_filters: Vec<(TopicFilter, QualityOfService)> = topics
                        .values()
                        .map(|mqtt_topic| {
                            (mqtt_topic.topic_filter.clone(), QualityOfService::Level0)
                        })
                        .collect();
                    info!("Resubscribing to channels {:?} ...", channel_filters);
                    let sub = SubscribePacket::new(2, channel_filters);
                    let mut buf = Vec::new();
                    if sub.encode(&mut buf).is_ok() && stream.write_all(&buf[..]).await.is_err() {
                        warn!("Error writing subscribe packet");
                    }
                } else {
                    break;
                }
            }
        });
        Ok(())
    }

    pub async fn add_topic(&self, topic: Topic) -> anyhow::Result<()> {
        // TODO this could contain a oneshot with a result showing if subscribing was successful
        let (tx, rx) = oneshot::channel();
        self.topic_events_tx
            .send(JoinOrLeaveEvent::Join(topic, tx))
            .await?;
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
            .send(JoinOrLeaveEvent::Leave(topic, tx))
            .await?;

        if let Ok(true) = rx.await {
            Ok(())
        } else {
            Err(anyhow!("Failed to add topic"))
        }
    }
}

// Attempt to decrypt an announce message from another peer
fn decrypt_using_topic(payload: &[u8], topic: &Topic) -> Option<AnnounceAddress> {
    if let Some(announce_message_bytes) = topic.decrypt(payload) {
        let announce_address_result: Result<AnnounceAddress, Box<bincode::ErrorKind>> =
            deserialize(&announce_message_bytes);
        if let Ok(announce_address) = announce_address_result {
            return Some(announce_address);
        }
    }
    None
}

#[derive(Hash, Eq, PartialEq)]
struct MqttTopic {
    // topic: Topic,
    publish_packet: Vec<u8>,
    topic_filter: TopicFilter,
}

impl MqttTopic {
    fn new(topic: &Topic, announce_address: &AnnounceAddress) -> anyhow::Result<Self> {
        let topic_filter = TopicFilter::new(format!("hdp/{}", topic.public_id))?;
        let publish_packet = create_publish_packet(topic, announce_address)?;
        Ok(Self {
            publish_packet,
            topic_filter,
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

/// Connect to the MQTT server and send a connect packet
async fn connect(server_addr: &SocketAddr, client_id: String) -> anyhow::Result<TcpStream> {
    info!("Connecting to MQTT broker {:?} ... ", server_addr);
    let mut stream = tokio::time::timeout(TCP_TIMEOUT, TcpStream::connect(server_addr)).await??;

    // Send a connect packet and check the response
    let mut connect_packet = ConnectPacket::new(client_id);
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
    Ok(stream)
}

fn mqtt_dns_resolve(server: &str) -> anyhow::Result<SocketAddr> {
    let server_addr = server
        .to_socket_addrs()?
        .find(|x| x.is_ipv4())
        .ok_or_else(|| anyhow!("Failed to get IP of MQTT server"))?;
    Ok(server_addr)
}
