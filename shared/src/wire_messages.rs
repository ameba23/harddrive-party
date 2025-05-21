//! Wire messages for communicating with other Peers
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use thiserror::Error;

// TODO read error

/// A request to a remote peer
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub enum Request {
    /// A request to read the remote peer's shared file index
    Ls(IndexQuery),
    /// A request to download a remote peer's file (or a portion of the file)
    Read(ReadQuery),
    /// Contact details of another peer
    AnnouncePeer(AnnouncePeer),
}

/// A request to read the remote peer's shared file index
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Eq, Hash)]
pub struct IndexQuery {
    /// Base directory to query - defaults to all shared directories
    pub path: Option<String>,
    /// Filter term to search with
    pub searchterm: Option<String>,
    /// Whether to expand directories
    pub recursive: bool,
}

/// A request to download a remote peers file (or a portion of the file)
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Eq, Hash)]
pub struct ReadQuery {
    /// Path of the requested file
    pub path: String,
    /// Offset to start reading
    pub start: Option<u64>,
    /// Offset to finish reading
    pub end: Option<u64>,
}

/// A response to a `Request::Ls(IndexQuery)`
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum LsResponse {
    /// The found files or directories if the query was successful
    Success(Vec<Entry>),
    Err(LsResponseError),
}

/// A file or directory entry in a share query response
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Entry {
    /// Path and filename
    pub name: String,
    /// Size in bytes
    pub size: u64,
    /// Whether this is a directory or a file
    pub is_dir: bool,
}

/// Error from making a share index query
#[derive(Error, Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum LsResponseError {
    #[error("Database error")]
    DbError,
    #[error("Path not found")]
    PathNotFound,
}

/// Details of an announced peer
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub struct AnnounceAddress {
    pub connection_details: PeerConnectionDetails,
    pub public_key: [u8; 32],
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone, Hash)]
pub struct AnnouncePeer {
    pub announce_address: AnnounceAddress,
    // TODO signature
}

impl AnnounceAddress {
    /// Deserialize bytes to an AnnounceAddress - doing this manually gives us a small saving over
    /// using bincode - meaning the addresses are slightly shorted
    pub fn from_bytes(input: Vec<u8>) -> Result<Self, AnnounceAddressDecodeError> {
        if input.len() < 33 {
            return Err(AnnounceAddressDecodeError::BadLength);
        }
        let public_key = &input[..32];
        let type_value = &input[32];
        let (type_value, ip, port) = if *type_value > 2 {
            if input.len() < 33 + 16 {
                return Err(AnnounceAddressDecodeError::BadLength);
            }
            // IPV6
            let ip_bytes = &input[33..33 + 16];
            let ip_u128 = u128::from_be_bytes(
                ip_bytes
                    .try_into()
                    .map_err(|_| AnnounceAddressDecodeError::BadLength)?,
            );
            let ip = IpAddr::V6(Ipv6Addr::from_bits(ip_u128));

            let type_value = type_value - 2;
            let port = if type_value == 1 {
                None
            } else {
                if input.len() < 33 + 16 + 2 {
                    return Err(AnnounceAddressDecodeError::BadLength);
                }
                let port_bytes = &input[33 + 16..33 + 16 + 2];
                Some(u16::from_be_bytes(
                    port_bytes
                        .try_into()
                        .map_err(|_| AnnounceAddressDecodeError::BadLength)?,
                ))
            };

            (type_value, ip, port)
        } else {
            if input.len() < 33 + 4 {
                return Err(AnnounceAddressDecodeError::BadLength);
            }
            let ip_bytes = &input[33..33 + 4];
            let ip_u32 = u32::from_be_bytes(
                ip_bytes
                    .try_into()
                    .map_err(|_| AnnounceAddressDecodeError::BadLength)?,
            );
            let ip = IpAddr::V4(Ipv4Addr::from_bits(ip_u32));

            let port = if *type_value == 1 {
                None
            } else {
                if input.len() < 33 + 4 + 2 {
                    return Err(AnnounceAddressDecodeError::BadLength);
                }
                let port_bytes = &input[33 + 4..33 + 4 + 2];
                Some(u16::from_be_bytes(
                    port_bytes
                        .try_into()
                        .map_err(|_| AnnounceAddressDecodeError::BadLength)?,
                ))
            };
            (*type_value, ip, port)
        };

        let connection_details = match type_value {
            0 => {
                let socket_addr = match ip {
                    IpAddr::V4(ip) => SocketAddr::V4(SocketAddrV4::new(ip, port.unwrap())),
                    IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(ip, port.unwrap(), 0, 0)),
                };
                PeerConnectionDetails::NoNat(socket_addr)
            }
            1 => PeerConnectionDetails::Symmetric(ip),
            2 => {
                let socket_addr = match ip {
                    IpAddr::V4(ip) => SocketAddr::V4(SocketAddrV4::new(ip, port.unwrap())),
                    IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(ip, port.unwrap(), 0, 0)),
                };
                PeerConnectionDetails::Asymmetric(socket_addr)
            }
            _ => return Err(AnnounceAddressDecodeError::UnrecognizedTypeValue),
        };

        Ok(AnnounceAddress {
            connection_details,
            public_key: public_key.try_into().unwrap(),
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
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
        let mut announce_address: Vec<u8> = Vec::new();

        announce_address.extend_from_slice(&self.public_key);
        announce_address.extend_from_slice(&[type_value]);
        announce_address.extend_from_slice(&ip);
        announce_address.extend_from_slice(&port);
        announce_address
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

#[derive(Error, Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum AnnounceAddressDecodeError {
    #[error("Bad length")]
    BadLength,
    #[error("Type value is invalid")]
    UnrecognizedTypeValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_address_encoding() {
        let announce_address = AnnounceAddress {
            connection_details: PeerConnectionDetails::NoNat("127.0.0.1:3000".parse().unwrap()),
            public_key: [1; 32],
        };
        let bytes = announce_address.to_bytes();
        let announce_address_2 = AnnounceAddress::from_bytes(bytes).unwrap();
        assert_eq!(announce_address, announce_address_2);
    }
}
