use crate::PeerId;
use base64::{prelude::BASE64_STANDARD_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use thiserror::Error;

/// Details of an announced peer
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub struct AnnounceAddress {
    pub connection_details: PeerConnectionDetails,
    pub public_key: PeerId,
}

impl AnnounceAddress {
    /// Deserialize bytes to an AnnounceAddress - doing this manually gives us a small saving over
    /// using bincode - meaning the addresses are slightly shorted
    pub fn from_string(input_string: String) -> Result<Self, AnnounceAddressDecodeError> {
        if !input_string.is_ascii() {
            return Err(AnnounceAddressDecodeError::BadLength);
        }
        let type_value = input_string
            .as_bytes()
            .last()
            .copied()
            .ok_or(AnnounceAddressDecodeError::BadLength)?
            .checked_sub(b'0')
            .ok_or(AnnounceAddressDecodeError::ParseInt)?;

        let suffux_length_bytes = match type_value {
            0 => 4 + 2,
            1 => 4,
            2 => 4 + 2,
            3 => 16 + 2,
            4 => 16,
            5 => 16 + 2,
            _ => return Err(AnnounceAddressDecodeError::UnrecognizedTypeValue),
        };
        let suffix_length_chars = (suffux_length_bytes * 8usize).div_ceil(6);
        let expected_length = PeerId::ENCODED_LENGTH + suffix_length_chars + 1;
        if input_string.len() != expected_length {
            return Err(AnnounceAddressDecodeError::BadLength);
        }
        let public_key = input_string[..PeerId::ENCODED_LENGTH].parse().map_err(
            |error: crate::peer_id::PeerIdParseError| {
                AnnounceAddressDecodeError::PublicKey(error.to_string())
            },
        )?;
        let truncated_string =
            &input_string[PeerId::ENCODED_LENGTH..PeerId::ENCODED_LENGTH + suffix_length_chars];
        let input = BASE64_STANDARD_NO_PAD.decode(truncated_string)?;
        let (type_value, ip, port) = if type_value > 2 {
            if input.len() < 16 {
                return Err(AnnounceAddressDecodeError::BadLength);
            }
            // IPV6
            let ip_bytes = &input[input.len() - 16..];
            let ip_u128 = u128::from_be_bytes(
                ip_bytes
                    .try_into()
                    .map_err(|_| AnnounceAddressDecodeError::BadLength)?,
            );
            let ip = IpAddr::V6(Ipv6Addr::from_bits(ip_u128));

            let type_value = type_value - 3;
            let port = if type_value == 1 {
                None
            } else {
                if input.len() < 16 + 2 {
                    return Err(AnnounceAddressDecodeError::BadLength);
                }
                let port_bytes = &input[input.len() - 16 - 2..input.len() - 16];

                Some(u16::from_be_bytes(
                    port_bytes
                        .try_into()
                        .map_err(|_| AnnounceAddressDecodeError::BadLength)?,
                ))
            };

            (type_value, ip, port)
        } else {
            if input.len() < 4 {
                return Err(AnnounceAddressDecodeError::BadLength);
            }
            let ip_bytes = &input[input.len() - 4..];
            let ip_u32 = u32::from_be_bytes(
                ip_bytes
                    .try_into()
                    .map_err(|_| AnnounceAddressDecodeError::BadLength)?,
            );
            let ip = IpAddr::V4(Ipv4Addr::from_bits(ip_u32));

            let port = if type_value == 1 {
                None
            } else {
                if input.len() < 4 + 2 {
                    return Err(AnnounceAddressDecodeError::BadLength);
                }

                let port_bytes = &input[input.len() - 4 - 2..input.len() - 4];

                Some(u16::from_be_bytes(
                    port_bytes
                        .try_into()
                        .map_err(|_| AnnounceAddressDecodeError::BadLength)?,
                ))
            };
            (type_value, ip, port)
        };

        let connection_details = match type_value {
            0 => {
                let port = port.ok_or(AnnounceAddressDecodeError::NoPort)?;
                let socket_addr = match ip {
                    IpAddr::V4(ip) => SocketAddr::V4(SocketAddrV4::new(ip, port)),
                    IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(ip, port, 0, 0)),
                };
                PeerConnectionDetails::NoNat(socket_addr)
            }
            1 => PeerConnectionDetails::Symmetric(ip),
            2 => {
                let port = port.ok_or(AnnounceAddressDecodeError::NoPort)?;
                let socket_addr = match ip {
                    IpAddr::V4(ip) => SocketAddr::V4(SocketAddrV4::new(ip, port)),
                    IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(ip, port, 0, 0)),
                };
                PeerConnectionDetails::Asymmetric(socket_addr)
            }
            _ => return Err(AnnounceAddressDecodeError::UnrecognizedTypeValue),
        };

        Ok(AnnounceAddress {
            connection_details,
            public_key,
        })
    }
}

impl std::fmt::Display for AnnounceAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut type_value = match self.connection_details {
            PeerConnectionDetails::NoNat(_) => 0,
            PeerConnectionDetails::Symmetric(_) => 1,
            PeerConnectionDetails::Asymmetric(_) => 2,
        };

        if self.connection_details.ip().is_ipv6() {
            type_value += 3;
        }
        let ip = match self.connection_details.ip() {
            IpAddr::V4(ip_v4) => {
                let ip_bits = ip_v4.to_bits();
                let ip_bytes = ip_bits.to_be_bytes();
                ip_bytes.to_vec()
            }
            IpAddr::V6(ip_v6) => {
                let ip_bits = ip_v6.to_bits();
                let ip_bytes = ip_bits.to_be_bytes();
                ip_bytes.to_vec()
            }
        };

        let port = match self.connection_details.port() {
            Some(port) => port.to_be_bytes().to_vec(),
            None => Vec::new(),
        };

        let mut connection_details: Vec<u8> = Vec::new();
        connection_details.extend_from_slice(&port);
        connection_details.extend_from_slice(&ip);
        let connection_details_string = BASE64_STANDARD_NO_PAD.encode(&connection_details);

        write!(
            f,
            "{}{}{}",
            self.public_key, connection_details_string, type_value,
        )
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
    #[error("Invalid public key: {0}")]
    PublicKey(String),
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
        let public_key = PeerId::new(std::array::from_fn(|i| i as u8));
        let announce_addresses = vec![
            // IPV4
            AnnounceAddress {
                connection_details: PeerConnectionDetails::NoNat("127.0.0.1:3000".parse().unwrap()),
                public_key,
            },
            AnnounceAddress {
                connection_details: PeerConnectionDetails::Symmetric("8.8.8.8".parse().unwrap()),
                public_key,
            },
            AnnounceAddress {
                connection_details: PeerConnectionDetails::Asymmetric(
                    "8.8.8.8:2000".parse().unwrap(),
                ),
                public_key,
            },
            // IPV6
            AnnounceAddress {
                connection_details: PeerConnectionDetails::NoNat(
                    "[2001:db8:85a3::8a2e:370:7334]:443".parse().unwrap(),
                ),
                public_key,
            },
            AnnounceAddress {
                connection_details: PeerConnectionDetails::Symmetric(
                    "2001:db8:85a3::8a2e:370:7334".parse().unwrap(),
                ),
                public_key,
            },
            AnnounceAddress {
                connection_details: PeerConnectionDetails::Asymmetric(
                    "[2001:db8:85a3::8a2e:370:7334]:443".parse().unwrap(),
                ),
                public_key,
            },
        ];

        for announce_address in announce_addresses {
            let string = announce_address.to_string();
            let announce_address_2 = AnnounceAddress::from_string(string).unwrap();
            assert_eq!(announce_address, announce_address_2);
        }
    }

    #[test]
    fn malformed_and_legacy_codes_are_rejected_without_panicking() {
        let id = PeerId::new([7; 32]);
        let valid = AnnounceAddress {
            public_key: id,
            connection_details: PeerConnectionDetails::NoNat("127.0.0.1:3000".parse().unwrap()),
        }
        .to_string();
        for invalid in [
            String::new(),
            "amberCloudYakEJLLAHEK2".to_string(),
            format!("{}{}", &valid[..valid.len() - 1], "9"),
            format!("+{}", &valid[1..]),
            valid[..valid.len() - 1].to_string(),
            format!("{valid}x"),
        ] {
            assert!(AnnounceAddress::from_string(invalid).is_err());
        }
    }
}
