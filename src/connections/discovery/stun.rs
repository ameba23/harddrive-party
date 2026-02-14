//! Public address / NAT type discovery using STUN
use anyhow::anyhow;
use harddrive_party_shared::wire_messages::PeerConnectionDetails;
use log::debug;
use rand::seq::SliceRandom;
use std::net::{SocketAddr, ToSocketAddrs};
use stunclient::StunClient;
use tokio::net::UdpSocket;

/// How many servers to try before giving up
const MAX_STUN_SERVER_ATTEMPTS: usize = 10;

/// Get our public address and NAT type using STUN
pub async fn stun_test(
    socket: &UdpSocket,
    stun_servers: Option<Vec<String>>,
) -> anyhow::Result<PeerConnectionDetails> {
    let (public_addr1, public_addr2) = multipe_attempt_stun_query(socket, stun_servers).await?;

    // TODO here we should loop over IPs of all network interfaces
    let addr = socket.local_addr()?;
    let has_nat = addr.ip() != public_addr1.ip();
    let is_symmetric = public_addr1 != public_addr2;

    let details = if !has_nat {
        PeerConnectionDetails::NoNat(public_addr2)
    } else if is_symmetric {
        PeerConnectionDetails::Symmetric(public_addr2.ip())
    } else {
        PeerConnectionDetails::Asymmetric(public_addr2)
    };

    debug!(
        "Local address: {addr:?}  Public address 1: {public_addr1:?} Public address 2: {public_addr2:?} NAT: {details:?}"
    );

    Ok(details)
}

/// Try a selection of the availble stun servers until we get two results from different servers
async fn multipe_attempt_stun_query(
    socket: &UdpSocket,
    stun_servers_override: Option<Vec<String>>,
) -> anyhow::Result<(SocketAddr, SocketAddr)> {
    let selected_stun_servers: Vec<String> = if let Some(stun_servers) = stun_servers_override {
        stun_servers
    } else {
        let mut stun_servers = STUN_SERVERS;
        let (selected_stun_servers, _) =
            stun_servers.partial_shuffle(&mut rand::thread_rng(), MAX_STUN_SERVER_ATTEMPTS);
        selected_stun_servers
            .iter()
            .map(|server| (*server).to_string())
            .collect()
    };

    if selected_stun_servers.is_empty() {
        return Err(anyhow!("No STUN servers provided"));
    }

    let mut first_test = None;

    for stun_server in &selected_stun_servers {
        debug!("Attempting connection to stun server {stun_server}");
        match first_test {
            None => {
                if let Ok((public_add1, stun_server1)) = stun_query(socket, stun_server).await {
                    first_test = Some((public_add1, stun_server1));
                }
            }
            Some((public_add1, stun_server1)) => {
                if let Ok((public_add2, stun_server2)) = stun_query(socket, stun_server).await {
                    if stun_server1 != stun_server2 {
                        return Ok((public_add1, public_add2));
                    }
                }
            }
        }
    }
    Err(anyhow!(
        "Could not connect to any stun server - maybe no internet connection"
    ))
}

/// Query a single stun server
async fn stun_query(
    socket: &UdpSocket,
    stun_server: &str,
) -> anyhow::Result<(SocketAddr, SocketAddr)> {
    let stun_server = stun_server
        .to_socket_addrs()?
        .find(|x| x.is_ipv4())
        .ok_or_else(|| anyhow!("Failed to get IP of stun server"))?;
    let stun_client1 = StunClient::new(stun_server);
    let public_addr1 = stun_client1.query_external_address_async(socket).await?;
    Ok((public_addr1, stun_server))
}

#[cfg(test)]
pub(crate) mod test_utils {
    use bytecodec::{DecodeExt, EncodeExt};
    use std::net::SocketAddr;
    use stun_codec::rfc5389::{attributes::XorMappedAddress, methods::BINDING, Attribute};
    use stun_codec::{Message, MessageClass, MessageDecoder, MessageEncoder};
    use tokio::net::UdpSocket;

    /// Spawn a local UDP STUN server for tests.
    /// If `mapped_address` is `Some`, that address is returned in XOR-MAPPED-ADDRESS.
    /// If `None`, the request source address is echoed as XOR-MAPPED-ADDRESS.
    pub(crate) async fn spawn_mock_stun_server(
        mapped_address: Option<SocketAddr>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let server_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_socket.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let mut decoder = MessageDecoder::<Attribute>::new();
            loop {
                let Ok((len, peer_addr)) = server_socket.recv_from(&mut buf).await else {
                    break;
                };
                let Ok(decoded) = decoder.decode_from_bytes(&buf[..len]) else {
                    continue;
                };
                let Ok(request) = decoded else {
                    continue;
                };
                if request.class() != MessageClass::Request || request.method() != BINDING {
                    continue;
                }

                let response_addr = mapped_address.unwrap_or(peer_addr);
                let mut response = Message::<Attribute>::new(
                    MessageClass::SuccessResponse,
                    BINDING,
                    request.transaction_id(),
                );
                response.add_attribute(Attribute::XorMappedAddress(XorMappedAddress::new(
                    response_addr,
                )));

                let mut encoder = MessageEncoder::new();
                let Ok(response_bytes) = encoder.encode_into_bytes(response) else {
                    continue;
                };
                if server_socket
                    .send_to(&response_bytes, peer_addr)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        (server_addr.to_string(), handle)
    }
}

#[cfg(test)]
mod tests {
    use super::test_utils::spawn_mock_stun_server;
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    async fn run_stun_with_servers(
        mapped_addr_server_1: SocketAddr,
        mapped_addr_server_2: SocketAddr,
        client_bind_addr: SocketAddr,
    ) -> PeerConnectionDetails {
        let (server1, handle1) = spawn_mock_stun_server(Some(mapped_addr_server_1)).await;
        let (server2, handle2) = spawn_mock_stun_server(Some(mapped_addr_server_2)).await;

        let client_socket = UdpSocket::bind(client_bind_addr).await.unwrap();
        let result = stun_test(&client_socket, Some(vec![server1, server2]))
            .await
            .unwrap();

        handle1.abort();
        handle2.abort();

        result
    }

    #[tokio::test]
    async fn test_stun_no_nat_local_servers() {
        let client_socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let local_addr = client_socket.local_addr().unwrap();

        let (server1, handle1) = spawn_mock_stun_server(Some(local_addr)).await;
        let (server2, handle2) = spawn_mock_stun_server(Some(local_addr)).await;
        let details = stun_test(&client_socket, Some(vec![server1, server2]))
            .await
            .unwrap();
        handle1.abort();
        handle2.abort();

        assert!(matches!(details, PeerConnectionDetails::NoNat(addr) if addr == local_addr));
    }

    #[tokio::test]
    async fn test_stun_asymmetric_local_servers() {
        let public_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)), 45000);
        let details = run_stun_with_servers(
            public_addr,
            public_addr,
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        )
        .await;

        assert!(matches!(details, PeerConnectionDetails::Asymmetric(addr) if addr == public_addr));
    }

    #[tokio::test]
    async fn test_stun_symmetric_local_servers() {
        let public_addr_1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)), 45000);
        let public_addr_2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2)), 45001);
        let details = run_stun_with_servers(
            public_addr_1,
            public_addr_2,
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        )
        .await;

        assert!(
            matches!(details, PeerConnectionDetails::Symmetric(ip) if ip == public_addr_2.ip())
        );
    }
}

/// Stun server list from https://github.com/pradt2/always-online-stun
/// Servers are chosen randomly from this list until two are found which work and resolve to
/// different IP addresses
const STUN_SERVERS: [&str; 82] = [
    "stun.bridesbay.com:3478",
    "stun.dcalling.de:3478",
    "stun.tula.nu:3478",
    "stun.bethesda.net:3478",
    "stun.nextcloud.com:443",
    "stun.baltmannsweiler.de:3478",
    "stun.sonetel.com:3478",
    "stun.stochastix.de:3478",
    "stun.linuxtrent.it:3478",
    "stun.framasoft.org:3478",
    "stun.zentauron.de:3478",
    "stun.sipthor.net:3478",
    "stun.radiojar.com:3478",
    "stun.sip.us:3478",
    "stun.engineeredarts.co.uk:3478",
    "stun.alpirsbacher.de:3478",
    "stun.voipia.net:3478",
    "stun.finsterwalder.com:3478",
    "stun.ncic.com:3478",
    "stun.telnyx.com:3478",
    "stun.fitauto.ru:3478",
    "stun.m-online.net:3478",
    "stun.mixvoip.com:3478",
    "stun.ru-brides.com:3478",
    "stun.graftlab.com:3478",
    "stun.myspeciality.com:3478",
    "stun.f.haeder.net:3478",
    "stun.voip.blackberry.com:3478",
    "stun.telviva.com:3478",
    "stun.kanojo.de:3478",
    "stun.peethultra.be:3478",
    "stun.genymotion.com:3478",
    "stun.atagverwarming.nl:3478",
    "stun.meetwife.com:3478",
    "stun.romancecompass.com:3478",
    "stun.skydrone.aero:3478",
    "stun.acronis.com:3478",
    "stun.lovense.com:3478",
    "stun.nextcloud.com:3478",
    "stun.thinkrosystem.com:3478",
    "stun.pure-ip.com:3478",
    "stun.moonlight-stream.org:3478",
    "stun.bcs2005.net:3478",
    "stun.ringostat.com:3478",
    "stun.ttmath.org:3478",
    "stun.voipgate.com:3478",
    "stun.vavadating.com:3478",
    "stun.hot-chilli.net:3478",
    "stun.3deluxe.de:3478",
    "stun.antisip.com:3478",
    "stun.verbo.be:3478",
    "stun.uabrides.com:3478",
    "stun.technosens.fr:3478",
    "stun.threema.ch:3478",
    "stun.signalwire.com:3478",
    "stun.diallog.com:3478",
    "stun.ukh.de:3478",
    "stun.kaseya.com:3478",
    "stun.annatel.net:3478",
    "stun.romaaeterna.nl:3478",
    "stun.business-isp.nl:3478",
    "stun.vomessen.de:3478",
    "stun.voztovoice.org:3478",
    "stun.cope.es:3478",
    "stun.axialys.net:3478",
    "stun.files.fm:3478",
    "stun.cellmail.com:3478",
    "stun.oncloud7.ch:3478",
    "stun.ipfire.org:3478",
    "stun.siptrunk.com:3478",
    "stun.bitburger.de:3478",
    "stun.poetamatusel.org:3478",
    "stun.yesdates.com:3478",
    "stun.godatenow.com:3478",
    "stun.siplogin.de:3478",
    "stun.healthtap.com:3478",
    "stun.freeswitch.org:3478",
    "stun.fmo.de:3478",
    "stun.sonetel.net:3478",
    "stun.frozenmountain.com:3478",
    "stun.geesthacht.de:3478",
    "stun.flashdance.cx:3478",
];
