//! Fetch tokens (ARCHITECTURE.md §7): `X-Dedan-Node: <label>.<minute>.<hmac>` with
//! `hmac = HMAC-SHA256(k_hour, "<minute>|<prefix>")` in hex, where the prefix is the client's /24 (IPv4) or
//! /48 (IPv6). The node reveals `k_hour` on chain after the hour; a token then verifies only against the
//! network the node actually saw, which ties an "up" result and an attestation to a real fetch from a real
//! place without putting anyone's full address on a public chain. The nginx mock mints the same string.

use crate::types::{KeyReveal, NodeId};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

pub const HEADER: &str = "X-Dedan-Node";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub node_id: NodeId,
    pub minute: u64,
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
        mac: mac(k_hour, minute, prefix),
    }
}

/// True when the token was minted by `reveal.node_id` in `reveal.hour` for a client in `prefix`.
pub fn verify(token: &Token, reveal: &KeyReveal, prefix: &str) -> bool {
    if token.node_id != reveal.node_id || token.minute / 60 != reveal.hour {
        return false;
    }
    let mut m =
        HmacSha256::new_from_slice(reveal.k_hour.as_slice()).expect("HMAC accepts any key length");
    m.update(format!("{}|{}", token.minute, prefix).as_bytes());
    m.verify_slice(&token.mac).is_ok()
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}",
            self.node_id,
            self.minute,
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
        assert_eq!(t.to_string(), format!("istanbul.29000000.{V4}"));
    }

    #[test]
    fn verifies_only_with_the_right_key_hour_and_prefix() {
        let token: Token = format!("istanbul.29000000.{V4}").parse().unwrap();
        let reveal = KeyReveal {
            node_id: NodeId::new("istanbul"),
            hour: 29_000_000 / 60,
            k_hour: B256::from(key()),
        };
        assert!(verify(&token, &reveal, "10.77.0.0/24"));
        assert!(!verify(&token, &reveal, "10.77.1.0/24"));
        assert!(!verify(
            &token,
            &KeyReveal {
                hour: reveal.hour + 1,
                ..reveal.clone()
            },
            "10.77.0.0/24"
        ));
        assert!(!verify(
            &token,
            &KeyReveal {
                node_id: NodeId::new("tokyo"),
                ..reveal.clone()
            },
            "10.77.0.0/24"
        ));
        assert!(!verify(
            &token,
            &KeyReveal {
                k_hour: B256::ZERO,
                ..reveal
            },
            "10.77.0.0/24"
        ));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!("istanbul.29000000".parse::<Token>().is_err());
        assert!(".1.00".parse::<Token>().is_err());
        assert!(format!("istanbul.1.{V4}.extra").parse::<Token>().is_err());
    }
}
