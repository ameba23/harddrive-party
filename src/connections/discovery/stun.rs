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
pub async fn stun_test(socket: &UdpSocket) -> anyhow::Result<PeerConnectionDetails> {
    let (public_addr1, public_addr2) = multipe_attempt_stun_query(socket).await?;

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
) -> anyhow::Result<(SocketAddr, SocketAddr)> {
    let mut stun_servers = STUN_SERVERS;
    let (selected_stun_servers, _) =
        stun_servers.partial_shuffle(&mut rand::thread_rng(), MAX_STUN_SERVER_ATTEMPTS);
    let mut first_test = None;

    for stun_server in selected_stun_servers {
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

/// Stun server list from https://github.com/pradt2/always-online-stun
/// Servers are chosen randomly from this list until two are found which work and resolve to
/// different IP addresses
const STUN_SERVERS: [&str; 93] = [
    "stun.baltmannsweiler.de:3478",
    "stun.talkho.com:3478",
    "stun.dcalling.de:3478",
    "stun.ipfire.org:3478",
    "stun.bethesda.net:3478",
    "stun.freeswitch.org:3478",
    "stun.siplogin.de:3478",
    "stun.linuxtrent.it:3478",
    "stun.sonetel.net:3478",
    "stun.poetamatusel.org:3478",
    "stun.lleida.net:3478",
    "stun.oncloud7.ch:3478",
    "stun.sipnet.com:3478",
    "stun.voip.blackberry.com:3478",
    "stun.finsterwalder.com:3478", // Possible issue - need to check
    "stun.technosens.fr:3478",
    "stun.zentauron.de:3478",
    "stun.engineeredarts.co.uk:3478",
    "stun.diallog.com:3478",
    "stun.imp.ch:3478",
    "stun.acronis.com:3478",
    "stun.sipnet.net:3478",
    "stun.jowisoftware.de:3478",
    "stun.sipnet.ru:3478",
    "stun.frozenmountain.com:3478",
    "stun.3deluxe.de:3478",
    "stun.peethultra.be:3478",
    "stun.telnyx.com:3478",
    "stun.bitburger.de:3478",
    "stun.antisip.com:3478",
    "stun.ru-brides.com:3478",
    "stun.romancecompass.com:3478",
    "stun.business-isp.nl:3478",
    "stun.fmo.de:3478",
    "stun.godatenow.com:3478",
    "stun.vavadating.com:3478",
    "stun.streamnow.ch:3478",
    "stun.ncic.com:3478",
    "stun.geesthacht.de:3478",
    "stun.peeters.com:3478",
    "stun.axialys.net:3478",
    "stun.allflac.com:3478",
    "stun.zepter.ru:3478",
    "stun.sip.us:3478",
    "stun.files.fm:3478",
    "stun.kaseya.com:3478",
    "stun.alpirsbacher.de:3478",
    "stun.ringostat.com:3478",
    "stun.avigora.fr:3478",
    "stun.threema.ch:3478",
    "stun.ukh.de:3478",
    "stun.lovense.com:3478",
    "stun.graftlab.com:3478",
    "stun.romaaeterna.nl:3478",
    "stun.pure-ip.com:3478",
    "stun.nextcloud.com:443",
    "stun.yesdates.com:3478",
    "stun.stochastix.de:3478",
    "stun.heeds.eu:3478",
    "stun.sonetel.com:3478",
    "stun.genymotion.com:3478",
    "stun.thinkrosystem.com:3478",
    "stun.voipia.net:3478",
    "stun.uabrides.com:3478",
    "stun.3wayint.com:3478",
    "stun.flashdance.cx:3478",
    "stun.myspeciality.com:3478",
    "stun.skydrone.aero:3478",
    "stun.1cbit.ru:3478",
    "stun.ttmath.org:3478",
    "stun.nanocosmos.de:3478",
    "stun.siptrunk.com:3478",
    "stun.moonlight-stream.org:3478",
    "stun.voipgate.com:3478",
    "stun.kanojo.de:3478",
    "stun.bridesbay.com:3478",
    "stun.cope.es:3478",
    "stun.dcalling.de:3478",
    "stun.annatel.net:3478",
    "stun.atagverwarming.nl:3478",
    "stun.signalwire.com:3478",
    "stun.healthtap.com:3478",
    "stun.nextcloud.com:3478",
    "stun.verbo.be:3478",
    "stun.fitauto.ru:3478",
    "stun.piratenbrandenburg.de:3478",
    "stun.hot-chilli.net:3478",
    "stun.mixvoip.com:3478",
    "stun.meetwife.com:3478",
    "stun.radiojar.com:3478",
    "stun.root-1.de:3478",
    "stun.m-online.net:3478",
    "stun.f.haeder.net:3478",
];
