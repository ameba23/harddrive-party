//! Public address / NAT type discovery using STUN

use super::PeerConnectionDetails;
use anyhow::anyhow;
use log::debug;
use std::net::ToSocketAddrs;
use stunclient::StunClient;
use tokio::net::UdpSocket;

/// Get our public address and NAT type using STUN
pub async fn stun_test(socket: &UdpSocket) -> anyhow::Result<PeerConnectionDetails> {
    // TODO have a list of public stun servers and choose two randomly
    // let stun_client1 = StunClient::with_google_stun_server();
    // let public_addr1 = stun_client1.query_external_address(socket)?;

    let stun_server = "stun.talkho.com:3478"
        .to_socket_addrs()?
        .find(|x| x.is_ipv4())
        .ok_or_else(|| anyhow!("Failed to get IP of stun server"))?;
    let stun_client1 = StunClient::new(stun_server);
    let public_addr1 = stun_client1.query_external_address_async(socket).await?;

    let stun_server = "stun.dcalling.de:3478"
        .to_socket_addrs()?
        .find(|x| x.is_ipv4())
        .ok_or_else(|| anyhow!("Failed to get IP of stun server"))?;
    let stun_client2 = StunClient::new(stun_server);
    let public_addr2 = stun_client2.query_external_address_async(socket).await?;

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
        "Local address: {:?}  Public address 1: {:?} Public address 2: {:?} NAT: {:?}",
        addr, public_addr1, public_addr2, details
    );

    Ok(details)
}
