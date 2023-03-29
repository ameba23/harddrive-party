//! Public address / NAT type discovery using STUN

use anyhow::anyhow;
use log::debug;
use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, ToSocketAddrs};
use stunclient::StunClient;

/// Get our public address and NAT type using STUN
pub async fn stun_test(socket: &tokio::net::UdpSocket) -> anyhow::Result<(SocketAddr, NatType)> {
    // TODO have a list of public stun servers and choose two randomly
    let stun_client1 = StunClient::with_google_stun_server();
    let public_addr1 = stun_client1.query_external_address_async(socket).await?;

    let stun_server = "stun2.l.google.com:19302"
        .to_socket_addrs()?
        .find(|x| x.is_ipv4())
        .ok_or_else(|| anyhow!("Failed to get IP of stun server"))?;
    let stun_client2 = StunClient::new(stun_server);
    let public_addr2 = stun_client2.query_external_address_async(socket).await?;

    // TODO here we should loop over IPs of all network interfaces
    let addr = socket.local_addr()?;
    let has_nat = addr.ip() != public_addr1.ip();
    let is_symmetric = public_addr1 != public_addr2;

    let nat_type = if !has_nat {
        NatType::NoNat
    } else if is_symmetric {
        NatType::Symmetric
    } else {
        NatType::Asymmetric
    };

    debug!(
        "Local address: {:?}  Public address 1: {:?} Public address 2: {:?} NAT: {:?}",
        addr, public_addr1, public_addr2, nat_type
    );

    Ok((public_addr2, nat_type))
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub enum NatType {
    NoNat = 1,
    Asymmetric = 2,
    Symmetric = 3,
}

// fn addresses_identical(ip1: IpAddr, ip2: IpAddr) -> bool {
//     ip1 == ip2
// match ip1 {
//     IpAddr::V4(ip_v4_addr1) => {
//         if let IpAddr::V4(ip_v4_addr2) = ip2 {
//             ip_v4_addr1 == ip_v4_addr2
//         } else {
//             false
//         }
//     }
//     IpAddr::V6(ip_v6_addr1) => {
//         if let IpAddr::V6(ip_v6_addr2) = ip2 {
//             ip_v6_addr1 == ip_v6_addr2
//         } else {
//             false
//         }
//     }
// }
