use base64::{prelude::BASE64_STANDARD_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use thiserror::Error;

/// Details of an announced peer
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub struct AnnounceAddress {
    pub connection_details: PeerConnectionDetails,
    pub name: String,
}

impl AnnounceAddress {
    /// Deserialize bytes to an AnnounceAddress - doing this manually gives us a small saving over
    /// using bincode - meaning the addresses are slightly shorted
    pub fn from_string(input_string: String) -> Result<Self, AnnounceAddressDecodeError> {
        let type_value: u8 = input_string
            .chars()
            .last()
            .ok_or(AnnounceAddressDecodeError::BadLength)?
            .to_string()
            .parse()
            .map_err(|_| AnnounceAddressDecodeError::ParseInt)?;

        let suffux_length_bytes = match type_value {
            0 => 4 + 2,
            1 => 4,
            2 => 4 + 2,
            3 => 16 + 2,
            4 => 16,
            5 => 16 + 2,
            _ => panic!("Bad type value"),
        };
        let suffix_length_chars = ((suffux_length_bytes * 8) + 5) / 6;
        let truncated_string = input_string
            [input_string.len() - 1 - suffix_length_chars..input_string.len() - 1]
            .to_string();
        let input = BASE64_STANDARD_NO_PAD.decode(truncated_string)?;
        let name = &input_string[..input_string.len() - 1 - suffix_length_chars].to_string();
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
            name: name.to_string(),
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
            self.name,
            connection_details_string,
            type_value.to_string()
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
