use base64::{prelude::BASE64_STANDARD_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use thiserror::Error;

/// Details of an announced peer
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub struct AnnounceAddress {
    connection_candidates: Vec<PeerConnectionDetails>,
    pub name: String,
}

impl AnnounceAddress {
    pub fn new(
        name: String,
        connection_details_ipv4: PeerConnectionDetails,
        connection_details_ipv6: Option<PeerConnectionDetails>,
    ) -> Self {
        debug_assert!(connection_details_ipv4.is_ipv4());
        let mut connection_candidates = vec![connection_details_ipv4];
        if let Some(v6) = connection_details_ipv6 {
            debug_assert!(v6.is_ipv6());
            connection_candidates.push(v6);
        }
        Self {
            name,
            connection_candidates,
        }
    }

    /// Only used to reconstruct after deserializing the internal candidate list
    /// (e.g. from storage). Prefer [`Self::new`] for ordinary construction.
    pub fn from_candidates(
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
        // TODO improve this — note the name reads inverted; returns true only
        // when we advertise a direct v4 endpoint.
        matches!(
            self.get_ipv4_candidate(),
            Some(PeerConnectionDetails::NoNat(_))
        )
    }

    pub fn from_string(input_string: String) -> Result<Self, AnnounceAddressDecodeError> {
        if input_string.is_empty() {
            return Err(AnnounceAddressDecodeError::BadLength);
        }
        let shape_char = input_string
            .chars()
            .last()
            .ok_or(AnnounceAddressDecodeError::BadLength)?;
        let shape = Shape::from_hex(shape_char)?;

        let body_bytes = shape.body_len();
        let base64_chars = base64_len_no_pad(body_bytes);
        if input_string.len() < 1 + base64_chars {
            return Err(AnnounceAddressDecodeError::BadLength);
        }
        let name_end = input_string.len() - 1 - base64_chars;
        let name = input_string[..name_end].to_string();
        let body_b64 = &input_string[name_end..input_string.len() - 1];
        let body = BASE64_STANDARD_NO_PAD.decode(body_b64)?;
        if body.len() != body_bytes {
            return Err(AnnounceAddressDecodeError::BadLength);
        }

        let mut cursor = Cursor::new(&body);
        let mut connection_candidates = Vec::with_capacity(shape.candidate_count());
        if let Some(variant) = shape.v4 {
            let ip = Ipv4Addr::from(cursor.take_array::<4>()?);
            connection_candidates.push(match variant {
                Variant::NoNat => PeerConnectionDetails::NoNat(SocketAddr::V4(SocketAddrV4::new(
                    ip,
                    cursor.take_u16()?,
                ))),
                Variant::Asymmetric => PeerConnectionDetails::Asymmetric(SocketAddr::V4(
                    SocketAddrV4::new(ip, cursor.take_u16()?),
                )),
                Variant::Symmetric => PeerConnectionDetails::Symmetric(IpAddr::V4(ip)),
            });
        }
        if let Some(variant) = shape.v6 {
            let ip = Ipv6Addr::from(cursor.take_array::<16>()?);
            connection_candidates.push(match variant {
                Variant::NoNat => {
                    let port = cursor.take_u16()?;
                    let scope_id = cursor.take_u32()?;
                    PeerConnectionDetails::NoNat(SocketAddr::V6(SocketAddrV6::new(
                        ip, port, 0, scope_id,
                    )))
                }
                Variant::Asymmetric => {
                    let port = cursor.take_u16()?;
                    let scope_id = cursor.take_u32()?;
                    PeerConnectionDetails::Asymmetric(SocketAddr::V6(SocketAddrV6::new(
                        ip, port, 0, scope_id,
                    )))
                }
                Variant::Symmetric => PeerConnectionDetails::Symmetric(IpAddr::V6(ip)),
            });
        }

        Ok(AnnounceAddress {
            name,
            connection_candidates,
        })
    }
}

impl std::fmt::Display for AnnounceAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut body = Vec::new();
        let mut shape = Shape { v4: None, v6: None };

        if let Some(v4) = self.get_ipv4_candidate() {
            shape.v4 = Some(v4.variant());
            match v4 {
                PeerConnectionDetails::NoNat(SocketAddr::V4(sa))
                | PeerConnectionDetails::Asymmetric(SocketAddr::V4(sa)) => {
                    body.extend_from_slice(&sa.ip().octets());
                    body.extend_from_slice(&sa.port().to_be_bytes());
                }
                PeerConnectionDetails::Symmetric(IpAddr::V4(ip)) => {
                    body.extend_from_slice(&ip.octets());
                }
                _ => unreachable!("get_ipv4_candidate returned non-v4"),
            }
        }
        if let Some(v6) = self.get_ipv6_candidate() {
            shape.v6 = Some(v6.variant());
            match v6 {
                PeerConnectionDetails::NoNat(SocketAddr::V6(sa))
                | PeerConnectionDetails::Asymmetric(SocketAddr::V6(sa)) => {
                    body.extend_from_slice(&sa.ip().octets());
                    body.extend_from_slice(&sa.port().to_be_bytes());
                    body.extend_from_slice(&sa.scope_id().to_be_bytes());
                }
                PeerConnectionDetails::Symmetric(IpAddr::V6(ip)) => {
                    body.extend_from_slice(&ip.octets());
                }
                _ => unreachable!("get_ipv6_candidate returned non-v6"),
            }
        }

        let base64 = BASE64_STANDARD_NO_PAD.encode(&body);
        write!(f, "{}{}{:x}", self.name, base64, shape.to_nibble())
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Variant {
    NoNat = 1,
    Asymmetric = 2,
    Symmetric = 3,
}

impl Variant {
    fn from_bits(bits: u8) -> Option<Self> {
        match bits & 0x3 {
            0 => None,
            1 => Some(Variant::NoNat),
            2 => Some(Variant::Asymmetric),
            3 => Some(Variant::Symmetric),
            _ => unreachable!(),
        }
    }

    /// Bytes following the IP for this variant (port only; v6 scope_id is added
    /// separately in [`Shape::body_len`] / encoding).
    fn tail_len(self) -> usize {
        match self {
            Variant::NoNat | Variant::Asymmetric => 2,
            Variant::Symmetric => 0,
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Shape {
    v4: Option<Variant>,
    v6: Option<Variant>,
}

impl Shape {
    fn from_hex(c: char) -> Result<Self, AnnounceAddressDecodeError> {
        let nibble = c
            .to_digit(16)
            .ok_or(AnnounceAddressDecodeError::UnrecognizedShape)? as u8;
        let v4 = Variant::from_bits(nibble);
        let v6 = Variant::from_bits(nibble >> 2);
        if v4.is_none() && v6.is_none() {
            return Err(AnnounceAddressDecodeError::UnrecognizedShape);
        }
        Ok(Shape { v4, v6 })
    }

    fn to_nibble(self) -> u8 {
        let v4 = self.v4.map(|v| v as u8).unwrap_or(0);
        let v6 = self.v6.map(|v| v as u8).unwrap_or(0);
        (v6 << 2) | v4
    }

    fn candidate_count(self) -> usize {
        self.v4.is_some() as usize + self.v6.is_some() as usize
    }

    fn body_len(self) -> usize {
        let v4 = self.v4.map(|v| 4 + v.tail_len()).unwrap_or(0);
        // v6 NoNat/Asymmetric also carries scope_id (4 bytes); Symmetric v6 is
        // just an IpAddr with no scope info.
        let v6 = self
            .v6
            .map(|v| 16 + v.tail_len() + if v.tail_len() > 0 { 4 } else { 0 })
            .unwrap_or(0);
        v4 + v6
    }
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], AnnounceAddressDecodeError> {
        if self.pos + N > self.buf.len() {
            return Err(AnnounceAddressDecodeError::BadLength);
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        self.pos += N;
        Ok(out)
    }
    fn take_u16(&mut self) -> Result<u16, AnnounceAddressDecodeError> {
        Ok(u16::from_be_bytes(self.take_array::<2>()?))
    }
    fn take_u32(&mut self) -> Result<u32, AnnounceAddressDecodeError> {
        Ok(u32::from_be_bytes(self.take_array::<4>()?))
    }
}

/// Chars produced by [`BASE64_STANDARD_NO_PAD`] for `n` input bytes
fn base64_len_no_pad(n: usize) -> usize {
    (n * 4).div_ceil(3)
}

#[derive(Error, Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum AnnounceAddressDecodeError {
    #[error("Bad length")]
    BadLength,
    #[error("Unrecognized shape marker")]
    UnrecognizedShape,
    #[error("Bad base64")]
    Base64(String),
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

    fn variant(&self) -> Variant {
        match self {
            PeerConnectionDetails::NoNat(_) => Variant::NoNat,
            PeerConnectionDetails::Asymmetric(_) => Variant::Asymmetric,
            PeerConnectionDetails::Symmetric(_) => Variant::Symmetric,
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

    /// Convert to and from string representation
    fn roundtrip(a: AnnounceAddress) {
        let s = a.to_string();
        let b = AnnounceAddress::from_string(s.clone())
            .unwrap_or_else(|e| panic!("decode failed for {s:?}: {e}"));
        assert_eq!(a, b, "roundtrip mismatch for {s:?}");
    }

    // Tests use struct-literal construction so that we can exercise v6-only
    // and multi-candidate shapes that AnnounceAddress::new() doesn't produce.
    fn addr(name: &str, candidates: Vec<PeerConnectionDetails>) -> AnnounceAddress {
        AnnounceAddress {
            name: name.to_string(),
            connection_candidates: candidates,
        }
    }

    #[test]
    fn roundtrip_v4_only() {
        for a in [
            addr(
                "foobar",
                vec![PeerConnectionDetails::NoNat(
                    "127.0.0.1:3000".parse().unwrap(),
                )],
            ),
            addr(
                "angryOstrich",
                vec![PeerConnectionDetails::Symmetric("8.8.8.8".parse().unwrap())],
            ),
            addr(
                "wagglingWallaby",
                vec![PeerConnectionDetails::Asymmetric(
                    "8.8.8.8:2000".parse().unwrap(),
                )],
            ),
        ] {
            roundtrip(a);
        }
    }

    #[test]
    fn roundtrip_v6_only() {
        for a in [
            addr(
                "foobar",
                vec![PeerConnectionDetails::NoNat(
                    "[2001:db8:85a3::8a2e:370:7334]:443".parse().unwrap(),
                )],
            ),
            addr(
                "angryOstrich",
                vec![PeerConnectionDetails::Symmetric(
                    "2001:db8:85a3::8a2e:370:7334".parse().unwrap(),
                )],
            ),
            addr(
                "wagglingWallaby",
                vec![PeerConnectionDetails::Asymmetric(
                    "[2001:db8:85a3::8a2e:370:7334]:443".parse().unwrap(),
                )],
            ),
        ] {
            roundtrip(a);
        }
    }

    #[test]
    fn roundtrip_v4_plus_v6() {
        roundtrip(AnnounceAddress::new(
            "dualstackDuck".to_string(),
            PeerConnectionDetails::NoNat("192.168.1.5:1234".parse().unwrap()),
            Some(PeerConnectionDetails::NoNat(
                "[2001:db8:85a3::8a2e:370:7334]:443".parse().unwrap(),
            )),
        ));
        roundtrip(AnnounceAddress::new(
            "mobileMouse".to_string(),
            PeerConnectionDetails::Asymmetric("1.2.3.4:9999".parse().unwrap()),
            Some(PeerConnectionDetails::Symmetric(
                "2001:db8::1".parse().unwrap(),
            )),
        ));
    }

    #[test]
    fn roundtrip_preserves_v6_scope_id() {
        let sa = SocketAddrV6::new("fe80::1".parse().unwrap(), 1234, 0, 7);
        let a = addr(
            "linkLocalLemur",
            vec![PeerConnectionDetails::NoNat(SocketAddr::V6(sa))],
        );
        roundtrip(a);
    }

    #[test]
    fn empty_input_errors() {
        assert_eq!(
            AnnounceAddress::from_string(String::new()).unwrap_err(),
            AnnounceAddressDecodeError::BadLength,
        );
    }

    #[test]
    fn zero_shape_errors() {
        assert_eq!(
            AnnounceAddress::from_string("x0".to_string()).unwrap_err(),
            AnnounceAddressDecodeError::UnrecognizedShape,
        );
    }

    #[test]
    fn non_hex_shape_errors() {
        assert_eq!(
            AnnounceAddress::from_string("abcz".to_string()).unwrap_err(),
            AnnounceAddressDecodeError::UnrecognizedShape,
        );
    }

    #[test]
    fn truncated_body_errors() {
        // shape=1 → v4 NoNat body = 6 bytes = 8 base64 chars, but we give 2.
        assert_eq!(
            AnnounceAddress::from_string("nameXY1".to_string()).unwrap_err(),
            AnnounceAddressDecodeError::BadLength,
        );
    }
}
