//! Node records from ENS (ARCHITECTURE.md §3, §6): a node is `<label>.dedan.eth`, its payout address is
//! the name's `addr(60)`, its config is the `dedan.*` text records and its endpoint is the name's DNS A/AAAA
//! records (ENSIP-6). Everything is read through the Universal Resolver per ENSIP-10, so the nameserver
//! follows the same path as any ENS client; `alloy-ens` supplies the name encoding.

use crate::types::{Class, LatLon, NodeId};
use alloy::primitives::{keccak256, Address, Bytes, B256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::{SolCall, SolValue};
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, Ipv6Addr};

sol! {
    #[sol(rpc)]
    interface IUniversalResolver {
        function resolve(bytes name, bytes data) external view returns (bytes result, address resolver);
    }
    interface ITextResolver {
        function text(bytes32 node, string key) external view returns (string);
    }
    interface IAddrResolver {
        function addr(bytes32 node) external view returns (address);
    }
    interface IDNSRecordResolver {
        function dnsRecord(bytes32 node, bytes32 name, uint16 resource) external view returns (bytes);
    }
}

pub const KEY_CLASS: &str = "dedan.class";
pub const KEY_LOCATION: &str = "dedan.location";
pub const KEY_PORT: &str = "dedan.port";
pub const KEY_ENCKEY: &str = "dedan.enckey";
pub const KEY_VERSION: &str = "dedan.version";
pub const RR_A: u16 = 1;
pub const RR_AAAA: u16 = 28;

/// What a node publishes about itself, as read from its name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NodeRecord {
    pub payout: Address,
    pub ipv4: Option<Ipv4Addr>,
    pub ipv6: Option<Ipv6Addr>,
    pub port: u16,
    pub class: Option<Class>,
    pub location: Option<LatLon>,
    pub enc_key: String,
    pub version: String,
}

pub type EnsError = Box<dyn std::error::Error + Send + Sync>;

/// The namehash of `<label>.<parent>`.
pub fn node_of(label: &NodeId, parent: &str) -> B256 {
    alloy_ens::namehash(&label.name(parent))
}

/// DNS wire encoding of `<label>.<parent>`, what the resolver keys DNS records by.
pub fn dns_name(label: &NodeId, parent: &str) -> Vec<u8> {
    alloy_ens::dns_encode(&label.name(parent))
}

/// One resource record of an RFC 1035 wire-format RRset: `(type, rdata)`.
pub fn parse_rrset(wire: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < wire.len() {
        // owner name: length-prefixed labels up to the zero label
        loop {
            let Some(&len) = wire.get(i) else { return out };
            i += 1;
            if len == 0 {
                break;
            }
            i += len as usize;
        }
        if i + 10 > wire.len() {
            return out;
        }
        let rtype = u16::from_be_bytes([wire[i], wire[i + 1]]);
        let rdlen = u16::from_be_bytes([wire[i + 8], wire[i + 9]]) as usize;
        i += 10;
        if i + rdlen > wire.len() {
            return out;
        }
        out.push((rtype, wire[i..i + rdlen].to_vec()));
        i += rdlen;
    }
    out
}

/// Reads node records through a Universal Resolver.
pub struct EnsClient<P> {
    provider: P,
    universal_resolver: Address,
    parent: String,
}

impl<P: Provider + Clone> EnsClient<P> {
    pub fn new(provider: P, universal_resolver: Address, parent: impl Into<String>) -> Self {
        Self {
            provider,
            universal_resolver,
            parent: parent.into(),
        }
    }

    pub fn parent(&self) -> &str {
        &self.parent
    }

    /// `resolve(name, data)` per ENSIP-10: the Universal Resolver finds the name's resolver and calls it.
    async fn resolve(&self, name: &[u8], data: Vec<u8>) -> Result<Option<Bytes>, EnsError> {
        let ur = IUniversalResolver::new(self.universal_resolver, self.provider.clone());
        match ur
            .resolve(Bytes::copy_from_slice(name), Bytes::from(data))
            .call()
            .await
        {
            Ok(r) => Ok(Some(r.result)),
            // an unset record answers with empty data on some paths and reverts on others; both mean "none"
            Err(_) => Ok(None),
        }
    }

    async fn text(&self, name: &[u8], node: B256, key: &str) -> Result<String, EnsError> {
        let data = ITextResolver::textCall {
            node,
            key: key.to_owned(),
        }
        .abi_encode();
        match self.resolve(name, data).await? {
            Some(raw) if !raw.is_empty() => Ok(String::abi_decode(&raw)?),
            _ => Ok(String::new()),
        }
    }

    async fn dns_record(
        &self,
        name: &[u8],
        node: B256,
        resource: u16,
    ) -> Result<Vec<u8>, EnsError> {
        let data = IDNSRecordResolver::dnsRecordCall {
            node,
            name: keccak256(name),
            resource,
        }
        .abi_encode();
        match self.resolve(name, data).await? {
            Some(raw) if !raw.is_empty() => Ok(Bytes::abi_decode(&raw)?.to_vec()),
            _ => Ok(Vec::new()),
        }
    }

    /// Every record of `<label>.<parent>`; missing records are empty or `None`, never an error.
    pub async fn node_record(&self, label: &NodeId) -> Result<NodeRecord, EnsError> {
        let name = dns_name(label, &self.parent);
        let node = node_of(label, &self.parent);
        let payout = match self
            .resolve(&name, IAddrResolver::addrCall { node }.abi_encode())
            .await?
        {
            Some(raw) if !raw.is_empty() => Address::abi_decode(&raw)?,
            _ => Address::ZERO,
        };
        let a = self.dns_record(&name, node, RR_A).await?;
        let aaaa = self.dns_record(&name, node, RR_AAAA).await?;
        let ipv4 = parse_rrset(&a)
            .into_iter()
            .find(|(t, _)| *t == RR_A)
            .and_then(|(_, rd)| <[u8; 4]>::try_from(rd.as_slice()).ok().map(Ipv4Addr::from));
        let ipv6 = parse_rrset(&aaaa)
            .into_iter()
            .find(|(t, _)| *t == RR_AAAA)
            .and_then(|(_, rd)| <[u8; 16]>::try_from(rd.as_slice()).ok().map(Ipv6Addr::from));
        let port = self
            .text(&name, node, KEY_PORT)
            .await?
            .trim()
            .parse()
            .unwrap_or(443);
        Ok(NodeRecord {
            payout,
            ipv4,
            ipv6,
            port,
            class: Class::parse_record(&self.text(&name, node, KEY_CLASS).await?),
            location: LatLon::parse_record(&self.text(&name, node, KEY_LOCATION).await?),
            enc_key: self.text(&name, node, KEY_ENCKEY).await?,
            version: self.text(&name, node, KEY_VERSION).await?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_encode_like_ens() {
        let fra = NodeId::new("frankfurt");
        assert_eq!(
            dns_name(&fra, "dedan.eth"),
            b"\x09frankfurt\x05dedan\x03eth\x00"
        );
        // namehash("eth") is the well-known constant
        assert_eq!(
            format!("{}", alloy_ens::namehash("eth")),
            "0x93cdeb708b7545dc668eb9280176169d1c33cfd8ed6f04690a0bcc88a93fc4ae"
        );
        assert_ne!(
            node_of(&fra, "dedan.eth"),
            node_of(&NodeId::new("tokyo"), "dedan.eth")
        );
    }

    #[test]
    fn rrsets_parse() {
        let mut wire = b"\x09frankfurt\x05dedan\x03eth\x00".to_vec();
        wire.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 10, 77, 0, 11]);
        let mut second = wire.clone();
        second.extend_from_slice(b"\x09frankfurt\x05dedan\x03eth\x00");
        second.extend_from_slice(&[0, 28, 0, 1, 0, 0, 0, 60, 0, 16]);
        second.extend_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let rrs = parse_rrset(&second);
        assert_eq!(rrs.len(), 2);
        assert_eq!(rrs[0], (RR_A, vec![10, 77, 0, 11]));
        assert_eq!(rrs[1].0, RR_AAAA);
        assert!(
            parse_rrset(&wire[..20]).is_empty(),
            "truncated records are ignored"
        );
        assert!(parse_rrset(b"").is_empty());
    }

    /// Needs a devnet with the nodes registered: `make devnet`, then
    /// `cargo test -p dedan-core -- --ignored resolves_the_city_nodes`.
    #[tokio::test]
    #[ignore]
    async fn resolves_the_city_nodes() {
        use alloy::providers::ProviderBuilder;
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let dep: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{root}/contracts/deployments/devnet.json")).unwrap(),
        )
        .unwrap();
        let nodes: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{root}/deploy/devnet/nodes.json")).unwrap(),
        )
        .unwrap();
        let ur: Address = dep["universalResolver"].as_str().unwrap().parse().unwrap();
        let url = std::env::var("DEDAN_RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8545".into());
        let provider = ProviderBuilder::new().connect_http(url.parse().unwrap());
        let ens = EnsClient::new(provider, ur, "dedan.eth");
        for n in nodes["nodes"].as_array().unwrap() {
            let label = NodeId::new(n["label"].as_str().unwrap());
            let rec = ens.node_record(&label).await.unwrap();
            assert_eq!(
                rec.ipv4.unwrap().to_string(),
                n["ip"].as_str().unwrap(),
                "{label}"
            );
            assert_eq!(rec.port, n["port"].as_u64().unwrap() as u16);
            assert_eq!(rec.class, Class::parse_record(n["class"].as_str().unwrap()));
            assert!(
                rec.location.is_some()
                    && rec.payout != Address::ZERO
                    && rec.version == "devnet-mock"
            );
        }
        assert_eq!(
            ens.node_record(&NodeId::new("nobody")).await.unwrap(),
            NodeRecord {
                port: 443,
                ..Default::default()
            },
            "an unregistered name has no records"
        );
    }
}
