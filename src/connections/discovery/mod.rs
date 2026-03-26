//! Peer discovery
use self::{
    hole_punch::{birthday_hard_side, PunchingUdpSocket},
    mdns::MdnsServer,
    stun::stun_test,
};
use crate::{
    connections::known_peers::KnownPeers, errors::UiServerErrorWrapper, peer::Peer,
    wire_messages::AnnounceAddress,
};
use harddrive_party_shared::{ui_messages::UiServerError, wire_messages::PeerConnectionDetails};
use hole_punch::HolePuncher;
use local_ip_address::local_ip;
use log::{debug, error, info, warn};
use quinn::AsyncUdpSocket;
use rand::Rng;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{atomic::{AtomicU64, Ordering}, Arc, RwLock},
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::{
        mpsc::{channel, Receiver, Sender},
        oneshot, Mutex,
    },
    time::sleep,
};

pub mod hole_punch;
pub mod mdns;
pub mod stun;

#[cfg(not(test))]
const GOSSIP_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
#[cfg(test)]
const GOSSIP_RETRY_BASE_DELAY: Duration = Duration::from_millis(25);

#[cfg(not(test))]
const GOSSIP_RETRY_MAX_DELAY: Duration = Duration::from_secs(300);
#[cfg(test)]
const GOSSIP_RETRY_MAX_DELAY: Duration = Duration::from_millis(200);

const GOSSIP_RETRY_MAX_ATTEMPTS: u32 = 10;

/// Details of a peer found through one of the discovery methods
#[derive(Debug)]
pub struct DiscoveredPeer {
    /// Socket address to connect to, or to expect an incoming connection from.
    pub socket_address: SocketAddr,
    /// Optional pre-bound socket needed for hard NAT traversal paths.
    pub socket_option: Option<UdpSocket>,
    /// Discovery source that produced this peer.
    pub discovery_method: DiscoveryMethod,
    /// The announced identity and connection details for the peer.
    pub announce_address: AnnounceAddress,
}

/// The way by which a peer was discovered
#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveryMethod {
    /// User provided the announce address
    Direct,
    /// Another peer provided the announce address
    Gossip,
    /// Discovered over local network
    Mdns,
}

/// Manages retry state for gossiped peers whose initial connection attempt failed.
#[derive(Clone)]
struct GossipRetryManager {
    /// Per-peer retry state keyed by announced peer name.
    state: Arc<Mutex<HashMap<String, GossipRetryState>>>,
    /// Channel used to re-enqueue a peer announcement for another discovery attempt.
    peer_announce_tx: Sender<PeerConnect>,
    /// Connected peers map used to suppress retries after a successful connection.
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    /// Monotonic token used to invalidate older scheduled retry tasks.
    generation: Arc<AtomicU64>,
}

/// In-memory retry metadata for a single gossiped peer.
#[derive(Clone, Debug)]
struct GossipRetryState {
    /// Latest announce details we should retry with.
    announce_address: AnnounceAddress,
    /// Number of retry attempts already scheduled for this peer.
    attempt: u32,
    /// Unique generation for the currently active retry chain.
    generation: u64,
    /// Instant when the next retry becomes eligible to fire.
    next_retry_at: Instant,
}

impl GossipRetryManager {
    /// Create a retry manager that can re-submit gossiped peer announcements locally.
    fn new(
        peer_announce_tx: Sender<PeerConnect>,
        peers: Arc<Mutex<HashMap<String, Peer>>>,
    ) -> Self {
        Self {
            state: Default::default(),
            peer_announce_tx,
            peers,
            generation: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Schedule a retry for a gossiped peer if the failure is transient and budget remains.
    async fn schedule_retry_if_needed(
        &self,
        discovery_method: DiscoveryMethod,
        announce_address: AnnounceAddress,
        err: &UiServerErrorWrapper,
    ) {
        if discovery_method != DiscoveryMethod::Gossip {
            return;
        }

        if !is_retryable_gossip_error(err) {
            info!(
                "Skipping gossip retry for {} due to non-retryable error: {}",
                announce_address.name, err
            );
            self.clear_retry(&announce_address.name).await;
            return;
        }

        if self.peers.lock().await.contains_key(&announce_address.name) {
            info!(
                "Skipping gossip retry for {} because the peer is already connected",
                announce_address.name
            );
            self.clear_retry(&announce_address.name).await;
            return;
        }

        let delay;
        let attempt;
        let generation;
        let peer_name = announce_address.name.clone();
        {
            let mut state = self.state.lock().await;
            // Every reschedule gets a new generation so any previously spawned sleep task
            // becomes a no-op when it wakes up.
            let next_generation = self.generation.fetch_add(1, Ordering::Relaxed);
            let next_attempt = state
                .get(&peer_name)
                .map_or(1, |existing| existing.attempt.saturating_add(1));

            if next_attempt > GOSSIP_RETRY_MAX_ATTEMPTS {
                warn!(
                    "Gossip retry budget exhausted for {} after {} attempts",
                    peer_name,
                    next_attempt - 1
                );
                state.remove(&peer_name);
                return;
            }

            delay = jittered_retry_delay(next_attempt);
            attempt = next_attempt;
            generation = next_generation;
            state.insert(
                peer_name.clone(),
                GossipRetryState {
                    announce_address: announce_address.clone(),
                    attempt,
                    generation,
                    next_retry_at: Instant::now() + delay,
                },
            );
        }

        info!(
            "Scheduled gossip retry for {} in {:?} (attempt {}/{})",
            peer_name, delay, attempt, GOSSIP_RETRY_MAX_ATTEMPTS
        );

        let manager = self.clone();
        tokio::spawn(async move {
            sleep(delay).await;
            manager.fire_retry(peer_name, generation).await;
        });
    }

    /// Re-submit a retry only if the stored generation is still the latest for that peer.
    async fn fire_retry(&self, peer_name: String, generation: u64) {
        if self.peers.lock().await.contains_key(&peer_name) {
            info!(
                "Skipping gossip retry for {} because the peer is already connected",
                peer_name
            );
            self.clear_retry(&peer_name).await;
            return;
        }

        let announce_address = {
            let state = self.state.lock().await;
            match state.get(&peer_name) {
                // Ignore stale tasks after a refresh/cancel, and only fire once the recorded
                // retry deadline has passed.
                Some(retry_state)
                    if retry_state.generation == generation
                        && retry_state.next_retry_at <= Instant::now() =>
                {
                    retry_state.announce_address.clone()
                }
                _ => return,
            }
        };

        info!(
            "Starting gossip retry for {} using {:?}",
            peer_name, announce_address.connection_details
        );
        if self
            .peer_announce_tx
            .send(PeerConnect {
                discovery_method: DiscoveryMethod::Gossip,
                announce_address,
                response_tx: None,
            })
            .await
            .is_err()
        {
            error!("Unable to enqueue gossip retry for {}", peer_name);
        }
    }

    /// Remove any queued retry state for a peer.
    async fn clear_retry(&self, peer_name: &str) {
        let removed = self.state.lock().await.remove(peer_name);
        if removed.is_some() {
            info!("Cleared gossip retry state for {}", peer_name);
        }
    }
}

/// Decide whether a gossip-triggered failure is worth retrying locally.
fn is_retryable_gossip_error(err: &UiServerErrorWrapper) -> bool {
    match &err.0 {
        UiServerError::PeerDiscovery(message) => {
            !matches!(
                message.as_str(),
                "Cannot connect to ourself"
                    | "Already connected to this peer"
                    | "Symmetric to symmetric not yet supported"
                    | "We have asymmetric NAT but no local socket"
            )
        }
        UiServerError::ConnectionError(message) => {
            let lower = message.to_lowercase();
            !lower.contains("certificate")
                && !lower.contains("bad signature")
                && !lower.contains("invalidcertificate")
        }
        _ => false,
    }
}

/// Calculate capped exponential backoff, then add a small jitter to avoid retry lockstep.
fn jittered_retry_delay(attempt: u32) -> Duration {
    let multiplier = 2u32.saturating_pow(attempt.saturating_sub(1));
    let base = GOSSIP_RETRY_BASE_DELAY.saturating_mul(multiplier);
    let capped = std::cmp::min(base, GOSSIP_RETRY_MAX_DELAY);
    jittered(capped)
}

/// Apply +/-20% jitter to a delay.
fn jittered(delay: Duration) -> Duration {
    let base_ms = delay.as_millis() as i64;
    let jitter_ms = (base_ms as f64 * 0.2) as i64;
    let offset = rand::thread_rng().gen_range(-jitter_ms..=jitter_ms);
    Duration::from_millis((base_ms + offset).max(0) as u64)
}

/// Handles the different peer discovery methods
pub struct PeerDiscovery {
    /// Channel on which discovery emits peers that should be connected to directly.
    pub peers_rx: Receiver<DiscoveredPeer>,
    /// Our own connection details
    pub announce_address: AnnounceAddress,
    /// Socket addresses we expect to see as incoming connections after a punch succeeds.
    pending_peer_connections: Arc<RwLock<HashMap<SocketAddr, (DiscoveryMethod, AnnounceAddress)>>>,
    /// Channel used to announce peers
    pub peer_announce_tx: Sender<PeerConnect>,
    /// Currently connected peers, used for dedupe and retry suppression.
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    /// Persistent store of previously discovered peers.
    pub known_peers: KnownPeers,
    /// Retry coordinator for failed gossiped peer connections.
    gossip_retry: GossipRetryManager,
}

impl PeerDiscovery {
    /// Build peer discovery, derive our public announce details, and start the announcement loop.
    pub async fn new(
        // Whether to use mDNS
        use_mdns: bool,
        public_key: [u8; 32],
        peers: Arc<Mutex<HashMap<String, Peer>>>,
        local_addr: Option<SocketAddr>,
        last_used_port: Option<u16>,
        known_peers_db: sled::Tree,
        stun_servers: Option<Vec<String>>,
    ) -> anyhow::Result<(Option<PunchingUdpSocket>, Self)> {
        // Channel for reporting discovered peers
        let (peers_tx, peers_rx) = channel(1024);

        // Channel for announcing peers to be handled
        let (peer_announce_tx, mut peer_announce_rx) = channel(1024);

        let mut local_addr = if let Some(local_addr) = local_addr {
            local_addr
        } else {
            SocketAddr::new(local_ip()?, 0)
        };

        // If port is unspecified, used the same port as last time
        if let Some(given_port) = last_used_port {
            if local_addr.port() == 0 {
                local_addr.set_port(given_port);
            }
        }

        // If we get an error with a given port try again with the port set to 0
        let raw_socket = if let Ok(socket) = UdpSocket::bind(local_addr).await {
            socket
        } else {
            warn!("Failed to bind to {local_addr} - trying another port");
            UdpSocket::bind(SocketAddr::new(local_addr.ip(), 0)).await?
        };

        // Get our public address and NAT type from a STUN server.
        // TODO make this offline-first by if we have an error and mDNS is enabled, ignore the
        // error
        let local_connection_details = stun_test(&raw_socket, stun_servers).await?;
        info!(
            "Peer discovery ready: local_bind={} announce_details={:?}",
            raw_socket.local_addr()?,
            local_connection_details
        );

        let (socket, hole_puncher) = PunchingUdpSocket::bind(raw_socket).await?;

        // Only use the hole_puncher if we are not behind symmetric nat
        let hole_puncher = match local_connection_details {
            PeerConnectionDetails::Symmetric(_) => None,
            _ => Some(hole_puncher.clone()),
        };

        let addr = socket.local_addr()?;

        // Id is used as an identifier for mdns services
        let id = hex::encode(public_key);
        let known_peers = KnownPeers::new(known_peers_db);
        let gossip_retry =
            GossipRetryManager::new(peer_announce_tx.clone(), peers.clone());

        let socket_option = match local_connection_details {
            // TODO probably need to give the ip address here
            PeerConnectionDetails::Symmetric(_) => None,
            _ => Some(socket),
        };

        let announce_address = AnnounceAddress {
            connection_details: local_connection_details.clone(),
            name: key_to_animal::key_to_name(&public_key),
        };

        // Only use mdns if we are on a local network
        let _mdns_server = if use_mdns && is_private(local_addr.ip()) {
            Some(
                MdnsServer::new(
                    &id,
                    addr,
                    peers_tx.clone(),
                    announce_address.clone(),
                    known_peers.clone(),
                )
                .await?,
            )
        } else {
            None
        };

        let peer_discovery = Self {
            peers_rx,
            announce_address,
            pending_peer_connections: Default::default(),
            peer_announce_tx,
            peers,
            known_peers: known_peers.clone(),
            gossip_retry: gossip_retry.clone(),
        };

        let pending_peer_connections = peer_discovery.pending_peer_connections.clone();
        let own_announce_address = peer_discovery.announce_address.clone();
        let peers = peer_discovery.peers.clone();
        let gossip_retry = peer_discovery.gossip_retry.clone();

        // In a separate task, loop over peer announcements
        tokio::spawn(async move {
            while let Some(peer_connect) = peer_announce_rx.recv().await {
                let discovery_method = peer_connect.discovery_method.clone();
                let peer_name = peer_connect.announce_address.name.clone();
                let announce_address = peer_connect.announce_address.clone();
                if discovery_method == DiscoveryMethod::Gossip {
                    // A fresh gossip announcement supersedes any previously queued retry.
                    gossip_retry.clear_retry(&peer_name).await;
                }
                info!(
                    "Handling {:?} peer announcement for {} ({:?})",
                    discovery_method,
                    peer_name,
                    announce_address.connection_details
                );
                let result = handle_peer_announcement(
                    hole_puncher.clone(),
                    own_announce_address.clone(),
                    peers_tx.clone(),
                    pending_peer_connections.clone(),
                    peers.clone(),
                    announce_address.clone(),
                    discovery_method.clone(),
                    known_peers.clone(),
                )
                .await;

                if let Some(response_tx) = peer_connect.response_tx {
                    let _ = response_tx.send(result);
                } else if let Err(err) = result {
                    warn!("Failed to handle gossiped peer announcement {err}");
                    gossip_retry
                        .schedule_retry_if_needed(
                            discovery_method,
                            announce_address,
                            &err,
                        )
                        .await;
                } else {
                    info!(
                        "Finished handling {:?} peer announcement for {}",
                        discovery_method, peer_name
                    );
                }
            }
        });

        Ok((socket_option, peer_discovery))
    }

    /// Look up and consume a pending incoming connection expectation for a punched peer.
    pub fn get_pending_peer(
        &self,
        socket_address: &SocketAddr,
    ) -> Option<(DiscoveryMethod, AnnounceAddress)> {
        if let Ok(mut connections) = self.pending_peer_connections.write() {
            connections.remove(socket_address)
        } else {
            // TODO can clear poison
            error!("Poisoned RwLock pending_peer_connections");
            None
        }
    }

    /// Decide whether to use client verification based on our NAT type
    ///
    /// If we are not behind NAT we allow connections without needing to
    /// know their announce address (containing public key) up front
    pub fn use_client_verification(&self) -> bool {
        !matches!(
            self.announce_address.connection_details,
            PeerConnectionDetails::NoNat(_)
        )
    }

    /// Schedule a retry for a gossiped peer after a discovery-stage failure.
    pub async fn schedule_gossip_retry_if_needed(
        &self,
        announce_address: AnnounceAddress,
        err: &UiServerErrorWrapper,
    ) {
        self.gossip_retry
            .schedule_retry_if_needed(DiscoveryMethod::Gossip, announce_address, err)
            .await;
    }

    /// Convenience wrapper for retry scheduling when the caller has a plain `UiServerError`.
    pub async fn schedule_gossip_retry_if_needed_ui(
        &self,
        announce_address: AnnounceAddress,
        err: &UiServerError,
    ) {
        self.schedule_gossip_retry_if_needed(announce_address, &UiServerErrorWrapper(err.clone()))
            .await;
    }

    /// Cancel any queued gossip retry for a peer that has connected or been superseded.
    pub async fn clear_gossip_retry(&self, peer_name: &str) {
        self.gossip_retry.clear_retry(peer_name).await;
    }
}

// Check if an IP appears to be private
fn is_private(ip: IpAddr) -> bool {
    if let IpAddr::V4(ip_v4_addr) = ip {
        ip_v4_addr.is_private()
    } else {
        // In the case of ipv6 we cant be sure
        false
    }
}

/// This is called when a peer is announced, either directly by the user or through a gossiped peer
/// announcement from another connected peer.
#[allow(clippy::too_many_arguments)]
pub async fn handle_peer_announcement(
    hole_puncher: Option<HolePuncher>,
    our_announce_address: AnnounceAddress,
    peers_tx: Sender<DiscoveredPeer>,
    pending_peer_connections: Arc<RwLock<HashMap<SocketAddr, (DiscoveryMethod, AnnounceAddress)>>>,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    their_announce_address: AnnounceAddress,
    discovery_method: DiscoveryMethod,
    known_peers: KnownPeers,
) -> Result<(), UiServerErrorWrapper> {
    known_peers.add_peer(&their_announce_address)?;
    info!(
        "Peer announcement received via {:?}: local={:?} remote={} ({:?})",
        discovery_method,
        our_announce_address.connection_details,
        their_announce_address.name,
        their_announce_address.connection_details
    );
    // Check it is not ourself
    if our_announce_address == their_announce_address {
        return Err(UiServerError::PeerDiscovery("Cannot connect to ourself".to_string()).into());
    }

    // Check that we are not already connected to this peer
    if peers
        .lock()
        .await
        .contains_key(&their_announce_address.name)
    {
        return Err(
            UiServerError::PeerDiscovery("Already connected to this peer".to_string()).into(),
        );
    }

    // TODO check that it is not already a pending peer connection
    debug!("Remote peer {their_announce_address:?}");
    return match handle_peer(
        hole_puncher.clone(),
        &our_announce_address.connection_details,
        their_announce_address.clone(),
        discovery_method.clone(),
    )
    .await?
    {
        (Some(discovered_peer), _) => {
            // We connect to them
            info!(
                "Peer announcement resolved to outgoing connection: peer={} addr={} via {:?}",
                discovered_peer.announce_address.name,
                discovered_peer.socket_address,
                discovered_peer.discovery_method
            );
            if peers_tx.send(discovered_peer).await.is_err() {
                error!("Cannot write to channel");
            }
            Ok(())
        }
        (None, socket_address) => {
            info!(
                "Peer announcement resolved to waiting for incoming connection: peer={} expected_remote_addr={}",
                their_announce_address.name, socket_address
            );
            // TODO should we clear poison here?
            // They connect to us
            pending_peer_connections
                .write()?
                .insert(socket_address, (discovery_method, their_announce_address));
            Ok(())
        }
    };
}

/// Handle a peer we have discovered - depending on NAT type
pub async fn handle_peer(
    hole_puncher: Option<HolePuncher>,
    local: &PeerConnectionDetails,
    announce_address: AnnounceAddress,
    discovery_method: DiscoveryMethod,
) -> Result<(Option<DiscoveredPeer>, SocketAddr), UiServerErrorWrapper> {
    match announce_address.connection_details {
        PeerConnectionDetails::Symmetric(remote_ip) => match local {
            PeerConnectionDetails::Symmetric(_) => Err(UiServerError::PeerDiscovery(
                "Symmetric to symmetric not yet supported".to_string(),
            )
            .into()),
            PeerConnectionDetails::Asymmetric(_) => match hole_puncher {
                Some(mut puncher) => {
                    info!(
                        "Starting asymmetric->symmetric hole punch: remote_peer={} remote_ip={}",
                        announce_address.name, remote_ip
                    );
                    let socket_address = puncher.hole_punch_peer_without_port(remote_ip).await?;
                    info!(
                        "Asymmetric->symmetric hole punch found remote sender for {} at {}",
                        announce_address.name, socket_address
                    );
                    // Wait for them to connect to us
                    Ok((None, socket_address))
                }
                None => Err(UiServerError::PeerDiscovery(
                    "We have asymmetric NAT but no local socket".to_string(),
                )
                .into()),
            },
            PeerConnectionDetails::NoNat(socket_address) => {
                // They are symmetric (hard), we have no nat
                // Wait for them to connect
                Ok((None, *socket_address))
            }
        },
        PeerConnectionDetails::Asymmetric(socket_address) => {
            match local {
                PeerConnectionDetails::Asymmetric(our_socket_address) => {
                    match hole_puncher {
                        Some(mut puncher) => {
                            if our_socket_address.ip() != socket_address.ip() {
                                info!(
                                    "Starting asymmetric->asymmetric hole punch: peer={} local_public_addr={} remote_public_addr={}",
                                    announce_address.name, our_socket_address, socket_address
                                );
                                puncher.hole_punch_peer(socket_address).await?;
                                info!(
                                    "Asymmetric->asymmetric hole punch completed for peer={} remote_public_addr={}",
                                    announce_address.name, socket_address
                                );
                            } else {
                                info!(
                                    "Skipping hole punch for peer={} because both peers share public IP {}",
                                    announce_address.name,
                                    our_socket_address.ip()
                                );
                            }
                            // Decide whether to connect or let them connect, by lexicographically
                            // comparing socket addresses
                            Ok(if our_socket_address > &socket_address {
                                (
                                    Some(DiscoveredPeer {
                                        discovery_method,
                                        announce_address,
                                        socket_address,
                                        socket_option: None,
                                    }),
                                    socket_address,
                                )
                            } else {
                                (None, socket_address)
                            })
                        }
                        None => Err(UiServerError::PeerDiscovery(
                            "We have asymmetric nat but no local socket".to_string(),
                        )
                        .into()),
                    }
                }
                PeerConnectionDetails::Symmetric(_) => {
                    info!(
                        "Starting birthday-paradox hard side: peer={} target_addr={}",
                        announce_address.name, socket_address
                    );
                    let (socket, socket_address) = birthday_hard_side(socket_address).await?;
                    info!(
                        "Birthday-paradox hard side obtained socket for peer={} target_addr={}",
                        announce_address.name, socket_address
                    );
                    Ok((
                        Some(DiscoveredPeer {
                            discovery_method,
                            announce_address,
                            socket_address,
                            socket_option: Some(socket),
                        }),
                        socket_address,
                    ))
                }
                PeerConnectionDetails::NoNat(socket_address) => {
                    // They are Asymmetric (easy), we have no nat
                    // just wait for them to connect
                    Ok((None, *socket_address))
                }
            }
        }
        PeerConnectionDetails::NoNat(socket_address) => {
            // They have no nat - should be able to connect to them normally
            match local {
                PeerConnectionDetails::NoNat(our_socket_address) => {
                    // Need to decide whether to connect
                    Ok(if our_socket_address > &socket_address {
                        (
                            Some(DiscoveredPeer {
                                discovery_method,
                                announce_address,
                                socket_address,
                                socket_option: None,
                            }),
                            socket_address,
                        )
                    } else {
                        (None, socket_address)
                    })
                }
                PeerConnectionDetails::Symmetric(_) => {
                    let socket = UdpSocket::bind("0.0.0.0:0")
                        .await
                        .map_err(|e| UiServerError::PeerDiscovery(e.to_string()))?;
                    Ok((
                        Some(DiscoveredPeer {
                            discovery_method,
                            announce_address,
                            socket_address,
                            socket_option: Some(socket),
                        }),
                        socket_address,
                    ))
                }
                _ => Ok((
                    Some(DiscoveredPeer {
                        discovery_method,
                        announce_address,
                        socket_address,
                        socket_option: None,
                    }),
                    socket_address,
                )),
            }
        }
    }
}

pub struct PeerConnect {
    /// Discovery source that produced this request.
    pub discovery_method: DiscoveryMethod,
    /// Peer details we want discovery to process now.
    pub announce_address: AnnounceAddress,
    /// Optional responder used by direct/manual connects that need a synchronous result.
    pub response_tx: Option<oneshot::Sender<Result<(), UiServerErrorWrapper>>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, timeout, Duration};

    fn announce(name: &str, addr: &str) -> AnnounceAddress {
        AnnounceAddress {
            connection_details: PeerConnectionDetails::Asymmetric(addr.parse().unwrap()),
            name: name.to_string(),
        }
    }

    #[test]
    fn classifies_retryable_gossip_errors() {
        assert!(is_retryable_gossip_error(&UiServerErrorWrapper(
            UiServerError::PeerDiscovery("Hole punching timed out".to_string()),
        )));
        assert!(is_retryable_gossip_error(&UiServerErrorWrapper(
            UiServerError::ConnectionError("transport closed".to_string()),
        )));
        assert!(!is_retryable_gossip_error(&UiServerErrorWrapper(
            UiServerError::PeerDiscovery("Already connected to this peer".to_string()),
        )));
        assert!(!is_retryable_gossip_error(&UiServerErrorWrapper(
            UiServerError::ConnectionError("InvalidCertificate(BadSignature)".to_string()),
        )));
    }

    #[tokio::test]
    async fn gossip_retry_uses_latest_announce_details() {
        let (tx, mut rx) = channel(8);
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let manager = GossipRetryManager::new(tx, peers);
        let err = UiServerErrorWrapper(UiServerError::ConnectionError("transport".to_string()));

        manager
            .schedule_retry_if_needed(DiscoveryMethod::Gossip, announce("carol", "127.0.0.1:1111"), &err)
            .await;
        manager
            .schedule_retry_if_needed(DiscoveryMethod::Gossip, announce("carol", "127.0.0.1:2222"), &err)
            .await;

        let retry = timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retry.announce_address, announce("carol", "127.0.0.1:2222"));

        sleep(Duration::from_millis(250)).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn clearing_retry_cancels_pending_send() {
        let (tx, mut rx) = channel(8);
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let manager = GossipRetryManager::new(tx, peers);
        let err = UiServerErrorWrapper(UiServerError::ConnectionError("transport".to_string()));

        manager
            .schedule_retry_if_needed(DiscoveryMethod::Gossip, announce("carol", "127.0.0.1:1111"), &err)
            .await;
        manager.clear_retry("carol").await;

        assert!(timeout(Duration::from_millis(400), rx.recv()).await.is_err());
    }
}
