use super::{hole_punch::HolePuncher, topic::Topic, DiscoveredPeer};
use anyhow::anyhow;
use bincode::{deserialize, serialize};
use log::{error, info, trace, warn};
use mqtt::{
    control::variable_header::ConnectReturnCode,
    packet::{
        ConnectPacket, PingreqPacket, PublishPacket, QoSWithPacketIdentifier, SubscribePacket,
        VariablePacket,
    },
    Encodable, QualityOfService, TopicFilter, TopicName,
};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, str, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpStream, sync::mpsc::UnboundedSender};

const KEEP_ALIVE: u16 = 10;

pub async fn mqtt_client(
    client_id: String,
    topics: Vec<Topic>,
    public_addr: SocketAddr,
    peers_tx: UnboundedSender<DiscoveredPeer>,
    hole_puncher: HolePuncher,
) -> anyhow::Result<()> {
    let server_addr: SocketAddr = "52.29.173.150:1883".parse()?; // broker.hivemq.com
    info!("Connecting to MQTT broker {:?} ... ", server_addr);
    let mut stream = TcpStream::connect(server_addr).await?;
    // let (mut mqtt_read, mut mqtt_write) = stream.split();

    info!("MQTT Client identifier {:?}", client_id);
    let mut conn = ConnectPacket::new(client_id.clone());
    conn.set_clean_session(true); // false?
    conn.set_keep_alive(KEEP_ALIVE);

    let will_topic = TopicName::new(format!("hdp/{}/{}", topics[0].public_id, client_id))?;
    conn.set_will(Some((will_topic, b"0".to_vec())));
    conn.set_will_retain(true);
    conn.set_will_qos(0);

    let mut buf = Vec::new();
    conn.encode(&mut buf)?;
    stream.write_all(&buf[..]).await?;

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
    let channel_filters: Vec<(TopicFilter, QualityOfService)> = topics
        .iter()
        .map(|t| {
            (
                TopicFilter::new(format!("hdp/{}/#", t.public_id)).unwrap(),
                QualityOfService::Level0,
            )
        })
        .collect();
    info!("Applying channel filters {:?} ...", channel_filters);
    let sub = SubscribePacket::new(10, channel_filters); // TODO the first arg is the packet id
    let mut buf = Vec::new();
    sub.encode(&mut buf)?;
    stream.write_all(&buf[..]).await?;

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
                panic!("SUBACK packet identifier not match");
            }

            info!("Subscribed!");
            break;
        }
    }

    // Publish our own address to chosen topics
    let announce_address = AnnounceAddress { public_addr };
    let announce_cleartext = serialize(&announce_address)?;
    for topic in &topics {
        let chan = TopicName::new(format!("hdp/{}/{}", topic.public_id, client_id))?;
        let encrypted_announce = topic.encrypt(&announce_cleartext);
        let mut publish_packet =
            PublishPacket::new(chan, QoSWithPacketIdentifier::Level0, encrypted_announce);
        publish_packet.set_retain(true);
        let mut buf = Vec::new();
        publish_packet.encode(&mut buf)?;
        stream.write_all(&buf[..]).await?;
    }

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
                                info!("Found our own message");
                                continue;
                            }
                            // Find the associated topic
                            if let Some(associated_topic) = topics.iter().find(|&topic| {
                                let tn = &publ.topic_name();
                                tn.contains(&topic.public_id)}
                            ) {
                                if let Some(announce_address) = decrypt_using_topic(&publ.payload().to_vec(), &associated_topic) {
                                    let mut hole_puncher_clone = hole_puncher.clone();
                                    tokio::spawn(async move {
                                        info!("Attempting hole punch...");
                                        if hole_puncher_clone.hole_punch_peer(announce_address.public_addr).await.is_err() {
                                            warn!("Hole punching failed");
                                        } else {
                                            info!("Hole punching succeeded");
                                        };
                                    });
                                    let us = public_addr.to_string();
                                    let them = announce_address.public_addr.to_string();
                                    if us > them {
                                        info!("PUBLISH ({})", publ.topic_name());
                                        if peers_tx
                                            .send(DiscoveredPeer {
                                                addr: announce_address.public_addr,
                                                token: None,
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
                () = tokio::time::sleep(Duration::from_secs(KEEP_ALIVE as u64 / 2)) => {
                    trace!("Sending PINGREQ to broker");

                    let pingreq_packet = PingreqPacket::new();

                    let mut buf = Vec::new();
                    if pingreq_packet.encode(&mut buf).is_err() {
                        error!("Cannot encode MQTT ping packet");
                        break;
                    };
                    if stream.write_all(&buf).await.is_err() {
                        error!("Cannot write MQTT ping packet");
                        break;
                    };
                }
            }
        }
    });

    Ok(())
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
struct AnnounceAddress {
    public_addr: SocketAddr,
}

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