//! Fetch tokens (ARCHITECTURE.md §7): `X-Dedan-Node: <label>.<minute>.<net>.<hmac>` with
//! `hmac = HMAC-SHA256(k_hour, "<minute>|<prefix>")` in hex, where the prefix is the client's /24 (IPv4) or
//! /48 (IPv6) as the node saw it and `<net>` is that prefix's network bytes in hex (6 or 12 characters). The
//! node reveals `k_hour` on chain after the hour; a token then verifies only for the network inside it, which
//! ties an "up" result and an attestation to a real fetch from a real place without putting anyone's full
//! address on a public chain. Carrying the network in the token means an auditor behind NAT reports the
//! prefix the node saw, not the one of its own interface.

use crate::types::{KeyReveal, NodeId};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

pub const HEADER: &str = "X-Dedan-Node";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub node_id: NodeId,
    pub minute: u64,
    /// The client's network as the node saw it, in [`prefix`] form: what the HMAC binds.
    pub prefix: String,
    pub mac: [u8; 32],
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("malformed fetch token")]
pub struct TokenError;

pub fn minute_of(ts: u64) -> u64 {
    ts / 60
}

pub fn hour_of(ts: u64) -> u64 {
    ts / 3600
}

/// The network a client is in: `10.77.0.0/24` for IPv4, `2001:db8:0::/48` for IPv6. The same string is what
/// the assignment uses to keep one node per network in a slot set.
pub fn prefix(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        IpAddr::V6(a) => {
            let s = a.segments();
            format!("{:x}:{:x}:{:x}::/48", s[0], s[1], s[2])
        }
    }
}

/// A [`prefix`] string as the token carries it: the network bytes in hex.
fn net_hex(prefix: &str) -> Option<String> {
    let (addr, len) = prefix.split_once('/')?;
    match len {
        "24" => {
            let o = addr.parse::<Ipv4Addr>().ok()?.octets();
            Some(hex::encode(&o[..3]))
        }
        "48" => {
            let s = addr.parse::<Ipv6Addr>().ok()?.segments();
            Some(format!("{:04x}{:04x}{:04x}", s[0], s[1], s[2]))
        }
        _ => None,
    }
}

/// The token's network back in [`prefix`] form.
fn prefix_of_net(net: &str) -> Option<String> {
    let b = hex::decode(net).ok()?;
    match b.len() {
        3 => Some(prefix(IpAddr::V4(Ipv4Addr::new(b[0], b[1], b[2], 0)))),
        6 => {
            let seg = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
            Some(prefix(IpAddr::V6(Ipv6Addr::new(
                seg(0),
                seg(2),
                seg(4),
                0,
                0,
                0,
                0,
                0,
            ))))
        }
        _ => None,
    }
}

pub fn mac(k_hour: &[u8; 32], minute: u64, prefix: &str) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(k_hour).expect("HMAC accepts any key length");
    m.update(format!("{minute}|{prefix}").as_bytes());
    let out = m.finalize().into_bytes();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

pub fn mint(node_id: &NodeId, minute: u64, k_hour: &[u8; 32], prefix: &str) -> Token {
    Token {
        node_id: node_id.clone(),
        minute,
        prefix: prefix.to_owned(),
        mac: mac(k_hour, minute, prefix),
    }
}

/// True when the token was minted by `reveal.node_id` in `reveal.hour` for a client in the token's network.
pub fn verify(token: &Token, reveal: &KeyReveal) -> bool {
    if token.node_id != reveal.node_id || token.minute / 60 != reveal.hour {
        return false;
    }
    let mut m =
        HmacSha256::new_from_slice(reveal.k_hour.as_slice()).expect("HMAC accepts any key length");
    m.update(format!("{}|{}", token.minute, token.prefix).as_bytes());
    m.verify_slice(&token.mac).is_ok()
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.node_id,
            self.minute,
            net_hex(&self.prefix).unwrap_or_default(),
            hex::encode(self.mac)
        )
    }
}

impl FromStr for Token {
    type Err = TokenError;

    fn from_str(s: &str) -> Result<Self, TokenError> {
        let mut parts = s.trim().split('.');
        let label = parts.next().filter(|l| !l.is_empty()).ok_or(TokenError)?;
        let minute = parts
            .next()
            .and_then(|p| p.parse().ok())
            .ok_or(TokenError)?;
        let prefix = parts.next().and_then(prefix_of_net).ok_or(TokenError)?;
        let raw = parts
            .next()
            .and_then(|p| hex::decode(p).ok())
            .ok_or(TokenError)?;
        if parts.next().is_some() || raw.len() != 32 {
            return Err(TokenError);
        }
        let mut mac = [0u8; 32];
        mac.copy_from_slice(&raw);
        Ok(Token {
            node_id: NodeId::new(label),
            minute,
            prefix,
            mac,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::B256;

    fn key() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        k
    }

    // Vectors computed independently with Python's hmac module:
    // hmac.new(bytes(range(1, 33)), b"29000000|10.77.0.0/24", hashlib.sha256).hexdigest()
    const V4: &str = "62246f13f3321cebbf4b7bb0e309ceeba43bdcc8aa013f16c2f5078daa296579";
    const V6: &str = "92b3c0fe941df6d111a10f031ca1a47c9dd183607ba06ac7c3f00e6c881a3930";

    #[test]
    fn prefixes() {
        assert_eq!(prefix("10.77.0.30".parse().unwrap()), "10.77.0.0/24");
        assert_eq!(prefix("2001:db8::1".parse().unwrap()), "2001:db8:0::/48");
    }

    #[test]
    fn matches_the_python_vectors() {
        assert_eq!(hex::encode(mac(&key(), 29_000_000, "10.77.0.0/24")), V4);
        assert_eq!(hex::encode(mac(&key(), 29_000_000, "2001:db8::/48")), V6);
        let t = mint(&NodeId::new("istanbul"), 29_000_000, &key(), "10.77.0.0/24");
        assert_eq!(t.to_string(), format!("istanbul.29000000.0a4d00.{V4}"));
    }

    #[test]
    fn the_network_travels_inside_the_token() {
        let v4 = mint(&NodeId::new("fra"), 7, &key(), "10.77.16.0/24");
        let back: Token = v4.to_string().parse().unwrap();
        assert_eq!(back, v4);
        assert_eq!(back.prefix, "10.77.16.0/24");
        let v6 = mint(&NodeId::new("fra"), 7, &key(), &prefix("2001:db8:ab::9".parse().unwrap()));
        let s = v6.to_string();
        assert!(s.starts_with("fra.7.20010db800ab."), "{s}");
        assert_eq!(s.parse::<Token>().unwrap().prefix, "2001:db8:ab::/48");
    }

    #[test]
    fn verifies_only_with_the_right_key_hour_and_network() {
        let token: Token = format!("istanbul.29000000.0a4d00.{V4}").parse().unwrap();
        let reveal = KeyReveal {
            node_id: NodeId::new("istanbul"),
            hour: 29_000_000 / 60,
            k_hour: B256::from(key()),
        };
        assert!(verify(&token, &reveal));
        let moved: Token = format!("istanbul.29000000.0a4d01.{V4}").parse().unwrap();
        assert!(!verify(&moved, &reveal), "another network, the same mac");
        assert!(!verify(
            &token,
            &KeyReveal {
                hour: reveal.hour + 1,
                ..reveal.clone()
            }
        ));
        assert!(!verify(
            &token,
            &KeyReveal {
                node_id: NodeId::new("tokyo"),
                ..reveal.clone()
            }
        ));
        assert!(!verify(
            &token,
            &KeyReveal {
                k_hour: B256::ZERO,
                ..reveal
            }
        ));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!("istanbul.29000000".parse::<Token>().is_err());
        assert!(".1.0a4d00.00".parse::<Token>().is_err());
        assert!(format!("istanbul.1.{V4}").parse::<Token>().is_err(), "the old form has no network");
        assert!(format!("istanbul.1.0a4d.{V4}").parse::<Token>().is_err());
        assert!(format!("istanbul.1.0a4d00.{V4}.extra").parse::<Token>().is_err());
    }
}
