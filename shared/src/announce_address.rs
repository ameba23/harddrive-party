use base64::{prelude::BASE64_STANDARD_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use thiserror::Error;

/// Details of an announced peer
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub struct AnnounceAddress {
    connection_candidates: Vec<PeerConnectionDetails>,
    pub name: String,
}

impl AnnounceAddress {
    /// Deserialize bytes to an AnnounceAddress - doing this manually gives us a small saving over
    /// using bincode - meaning the addresses are slightly shorted
    pub fn from_string(input_string: String) -> Result<Self, AnnounceAddressDecodeError> {
        let input = BASE64_STANDARD_NO_PAD.decode(input_string)?;
        let announce_address: AnnounceAddress = super::codec::deserialize(&input).unwrap();
        Ok(announce_address)
    }

    pub fn new(
        name: String,
        connection_details_ipv4: PeerConnectionDetails,
        connection_details_ipv6: Option<PeerConnectionDetails>,
    ) -> Self {
        // TODO assert that connection details are ipv4/IPV6
        let mut connection_candidates = vec![connection_details_ipv4];
        if let Some(connection_details_ipv6) = connection_details_ipv6 {
            connection_candidates.push(connection_details_ipv6);
        }
        Self {
            name,
            connection_candidates,
        }
    }

    // TODO this is only used when deserializing
    pub fn new_with_connection_candidates(
        name: String,
        connection_candidates: Vec<PeerConnectionDetails>,
    ) -> Self {
        Self {
            name,
            connection_candidates,
        }
    }

    pub fn get_ipv4_candidate(&self) -> Option<&PeerConnectionDetails> {
        self.connection_candidates.iter().find(|c| c.is_ipv4())
    }

    pub fn get_ipv6_candidate(&self) -> Option<&PeerConnectionDetails> {
        self.connection_candidates.iter().find(|c| c.is_ipv6())
    }

    pub fn connection_candidates(&self) -> &Vec<PeerConnectionDetails> {
        &self.connection_candidates
    }

    pub fn has_nat(&self) -> bool {
        // TODO improve this
        if let Some(PeerConnectionDetails::NoNat(_)) = self.get_ipv4_candidate() {
            true
        } else {
            false
        }
    }
}

impl std::fmt::Display for AnnounceAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bytes = super::codec::serialize(self).unwrap();
        let base64 = BASE64_STANDARD_NO_PAD.encode(&bytes);

        write!(f, "{}", base64)
    }
}

#[derive(Error, Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum AnnounceAddressDecodeError {
    #[error("Bad length")]
    BadLength,
    #[error("Type value is invalid")]
    UnrecognizedTypeValue,
    #[error("Cannot parse integer")]
    ParseInt,
    #[error("Bad base64")]
    Base64(String),
    #[error("No port given when one was expected")]
    NoPort,
}

impl From<base64::DecodeError> for AnnounceAddressDecodeError {
    fn from(error: base64::DecodeError) -> AnnounceAddressDecodeError {
        AnnounceAddressDecodeError::Base64(error.to_string())
    }
}

#[repr(u8)]
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub enum PeerConnectionDetails {
    NoNat(SocketAddr) = 1,
    Asymmetric(SocketAddr) = 2,
    Symmetric(IpAddr) = 3,
}

impl PeerConnectionDetails {
    /// Gets the IP address
    pub fn ip(&self) -> IpAddr {
        match self {
            PeerConnectionDetails::NoNat(addr) => addr.ip(),
            PeerConnectionDetails::Asymmetric(addr) => addr.ip(),
            PeerConnectionDetails::Symmetric(ip) => *ip,
        }
    }

    pub fn port(&self) -> Option<u16> {
        match self {
            PeerConnectionDetails::NoNat(addr) => Some(addr.port()),
            PeerConnectionDetails::Asymmetric(addr) => Some(addr.port()),
            PeerConnectionDetails::Symmetric(_) => None,
        }
    }

    pub fn is_ipv4(&self) -> bool {
        self.ip().is_ipv4()
    }

    pub fn is_ipv6(&self) -> bool {
        self.ip().is_ipv6()
    }
}

impl std::fmt::Display for PeerConnectionDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let nat_type = match self {
            PeerConnectionDetails::NoNat(_) => "No NAT",
            PeerConnectionDetails::Asymmetric(_) => "Asymmetric NAT",
            PeerConnectionDetails::Symmetric(_) => "Symmetric NAT",
        };
        let ip = self.ip().to_string();
        let port = self.port();
        let ip_port = match port {
            Some(port) => format!("{ip}:{port}"),
            None => ip.clone(),
        };

        write!(f, "{} {}", ip_port, nat_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_address_encoding() {
        let announce_addresses = vec![
            // IPV4
            AnnounceAddress {
                connection_details: PeerConnectionDetails::NoNat("127.0.0.1:3000".parse().unwrap()),
                name: "foobar".to_string(),
            },
            AnnounceAddress {
                connection_details: PeerConnectionDetails::Symmetric("8.8.8.8".parse().unwrap()),
                name: "angryOstrich".to_string(),
            },
            AnnounceAddress {
                connection_details: PeerConnectionDetails::Asymmetric(
                    "8.8.8.8:2000".parse().unwrap(),
                ),
                name: "wagglingWallaby".to_string(),
            },
            // IPV6
            AnnounceAddress {
                connection_details: PeerConnectionDetails::NoNat(
                    "[2001:db8:85a3::8a2e:370:7334]:443".parse().unwrap(),
                ),
                name: "foobar".to_string(),
            },
            AnnounceAddress {
                connection_details: PeerConnectionDetails::Symmetric(
                    "2001:db8:85a3::8a2e:370:7334".parse().unwrap(),
                ),
                name: "angryOstrich".to_string(),
            },
            AnnounceAddress {
                connection_details: PeerConnectionDetails::Asymmetric(
                    "[2001:db8:85a3::8a2e:370:7334]:443".parse().unwrap(),
                ),
                name: "wagglingWallaby".to_string(),
            },
        ];

        for announce_address in announce_addresses {
            let string = announce_address.to_string();
            let announce_address_2 = AnnounceAddress::from_string(string).unwrap();
            assert_eq!(announce_address, announce_address_2);
        }
    }
}
