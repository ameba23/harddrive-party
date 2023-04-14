//! Peer discovery by publishing ip address (encrypted with topic name) to an MQTT server

use super::{hole_punch::HolePuncher, stun::NatType, topic::Topic, DiscoveredPeer, SessionToken};
use anyhow::anyhow;
use bincode::{deserialize, serialize};
use log::{error, info, trace, warn};
use mqtt::{
    control::variable_header::ConnectReturnCode,
    packet::{
        ConnectPacket, PublishPacket, QoSWithPacketIdentifier, SubscribePacket, VariablePacket,
    },
    Encodable, QualityOfService, TopicFilter, TopicName,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{SocketAddr, ToSocketAddrs},
    str,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    sync::{mpsc::UnboundedSender, Mutex},
};

// Keep alive timeout in seconds
const KEEP_ALIVE: u16 = 30;
const TCP_TIMEOUT: Duration = Duration::from_secs(120);

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
        client_id: &str,
    ) -> anyhow::Result<Self> {
        let publish_packet = if publish {
            Some(create_publish_packet(topic, announce_address, client_id)?)
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
    client_id: &str,
) -> anyhow::Result<Vec<u8>> {
    let announce_cleartext = serialize(&announce_address)?;

    // TODO do we need a unique topic for each peer?
    let channel_name = TopicName::new(format!("hdp/{}/{}", topic.public_id, client_id))?;

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

pub struct MqttClient {
    topics: Arc<Mutex<HashMap<Topic, MqttTopic>>>,
    client_id: String,
    announce_address: AnnounceAddress,
}

impl MqttClient {
    pub async fn new(
        client_id: String,
        topics: Vec<Topic>,
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

        let client_id_clone = client_id.clone();
        let announce_address_clone = announce_address.clone();

        let mqtt_client = Self {
            topics: Default::default(),
            announce_address: announce_address_clone,
            client_id: client_id_clone,
        };

        for topic in topics {
            mqtt_client.add_topic(topic).await?;
        }

        mqtt_client.run(peers_tx, hole_puncher).await?;

        Ok(mqtt_client)
    }

    pub async fn run(
        &self,
        peers_tx: UnboundedSender<DiscoveredPeer>,
        hole_puncher: HolePuncher,
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
            trace!("CONNACK {:?}", connack);

            if connack.connect_return_code() != ConnectReturnCode::ConnectionAccepted {
                return Err(anyhow!(
                    "Failed to connect to server, return code {:?}",
                    connack.connect_return_code()
                ));
            }
        } else {
            return Err(anyhow!("Got unexpected packet - expecting Connack"));
        }

        // Subscribe to given topics
        {
            let topics = self.topics.lock().await;
            let channel_filters: Vec<(TopicFilter, QualityOfService)> = topics
                .keys()
                .map(|topic| {
                    (
                        TopicFilter::new(format!("hdp/{}/#", topic.public_id)).unwrap(),
                        QualityOfService::Level0,
                    )
                })
                .collect();
            info!("Applying channel filters {:?} ...", channel_filters);
            let sub = SubscribePacket::new(10, channel_filters); // TODO the first arg is the packet id
            let mut buf = Vec::new();
            sub.encode(&mut buf)?;
            stream.write_all(&buf[..]).await?;
        }

        // Check the response for the subscribe message
        loop {
            let packet = match VariablePacket::parse(&mut stream).await {
                Ok(pk) => pk,
                Err(err) => {
                    error!("Error in receiving packet {:?}", err);
                    continue;
                }
            };
            trace!("PACKET {:?}", packet);

            if let VariablePacket::SubackPacket(ref ack) = packet {
                if ack.packet_identifier() != 10 {
                    error!("SUBACK packet identifier did not match");
                } else {
                    info!("Subscribed!");
                }
                break;
            }
        }

        // Start a loop processing messages
        let topics_mutex = self.topics.clone();
        let client_id = self.client_id.clone();
        let announce_address = self.announce_address.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Ok(packet) = VariablePacket::parse(&mut stream) => {
                        trace!("PACKET {:?}", packet);

                        match packet {
                            VariablePacket::PingrespPacket(..) => {
                                trace!("Received PINGRESP from broker ..");
                            }
                            VariablePacket::PublishPacket(ref publ) => {
                                if publ.topic_name().ends_with(&client_id) {
                                    info!("Found our own announce message");
                                    continue;
                                }
                                // Find the associated topic
                                // TODO handle err
                                let topics = topics_mutex.lock().await;

                                if let Some(associated_topic) = topics.keys().find(|&topic| {
                                    let tn = &publ.topic_name();
                                    tn.contains(&topic.public_id)
                                }) {
                                    if let Some(remote_peer_announce) = decrypt_using_topic(&publ.payload().to_vec(), associated_topic) {
                                        // TODO dont connect if we are both on the same IP - use mdns
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
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // // Send ping packets to keep the connection open
                    // () = tokio::time::sleep(Duration::from_secs(KEEP_ALIVE as u64 / 2)) => {
                    //     trace!("Sending PINGREQ to broker");
                    //
                    //     let pingreq_packet = PingreqPacket::new();
                    //
                    //     let mut pingreq_packet_buf = Vec::new();
                    //     if pingreq_packet.encode(&mut pingreq_packet_buf).is_err() {
                    //         error!("Cannot encode MQTT ping packet");
                    //         break;
                    //     };
                    //     if stream.write_all(&pingreq_packet_buf).await.is_err() {
                    //         error!("Cannot write MQTT ping packet");
                    //         // break;
                    //     };
                    // }
                    // Send publish packets
                    () = tokio::time::sleep(Duration::from_secs(KEEP_ALIVE as u64 / 2)) => {
                        // TODO this should be run immediately as well as after delay
                        info!("Sending publish packets");
                        let topics = topics_mutex.lock().await;
                        // For each topic in the set, send publish message
                        for (_topic, mqtt_topic) in topics.iter() {
                            if let Some(publish_packet) = &mqtt_topic.publish_packet {
                                if let Err(e) = stream.write_all(&publish_packet[..]).await {
                                    error!("Error when writing to mqtt broker {:?}", e);
                                    break;
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
        // TODO publish a subscribe message
        let mqtt_topic =
            MqttTopic::new(&topic, true, true, &self.announce_address, &self.client_id)?;
        let mut topics = self.topics.lock().await;
        topics.insert(topic, mqtt_topic);
        Ok(())
    }

    pub async fn remove_topic(&self, topic: Topic) -> anyhow::Result<()> {
        // TODO publish an unsubscribe message
        let mut topics = self.topics.lock().await;
        topics.remove(&topic);
        Ok(())
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
