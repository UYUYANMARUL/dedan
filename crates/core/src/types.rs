//! Identifiers and the message payloads of ARCHITECTURE.md §11. Every off-chain message travels inside an
//! [`crate::envelope::Envelope`]; attestations and key reveals are events of the Dedan registry contract.
//! Money is `u128` micro-USDC (6 decimals) everywhere in this crate.

use alloy::primitives::{keccak256, Address, B256};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A node is its ENSv2 name: `<label>.dedan.eth`. The id is the label.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn new(label: impl Into<String>) -> Self {
        Self(label.into())
    }

    /// The envelope signer string, `node:<label>`.
    pub fn signer(&self) -> String {
        format!("node:{}", self.0)
    }

    /// `keccak256(label)`: the registry's label id and the `labelHash` in its events.
    pub fn label_hash(&self) -> B256 {
        keccak256(self.0.as_bytes())
    }

    /// The full ENS name under the parent, e.g. `frankfurt.dedan.eth`.
    pub fn name(&self, parent: &str) -> String {
        format!("{}.{}", self.0, parent)
    }
}

impl From<&str> for NodeId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! address_id {
    ($name:ident, $prefix:literal) => {
        /// An account that holds a role on the Dedan registry.
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Hash,
            PartialOrd,
            Ord,
            Serialize,
            Deserialize,
            Default,
        )]
        #[serde(transparent)]
        pub struct $name(pub Address);

        impl $name {
            /// The envelope signer string, e.g. `auditor:0xabc…`.
            pub fn signer(self) -> String {
                format!("{}:{}", $prefix, self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

address_id!(AuditorId, "auditor");
address_id!(ResolverId, "resolver");

/// The account label: `<label>.dedan-cdn.net`, `bytes32(label)` on-chain.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct AccountId(pub String);

impl AccountId {
    pub fn new(label: impl Into<String>) -> Self {
        Self(label.into())
    }

    pub fn signer(&self) -> String {
        format!("account:{}", self.0)
    }

    /// `bytes32("acme")` as Solidity encodes a string literal: left-aligned, zero-padded on the right.
    pub fn bytes32(&self) -> B256 {
        let mut out = [0u8; 32];
        let src = self.0.as_bytes();
        let n = src.len().min(32);
        out[..n].copy_from_slice(&src[..n]);
        B256::from(out)
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An H3 cell index (resolution 5, see [`crate::geo`]).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct Cell(pub u64);

impl fmt::Display for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    pub fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon }
    }

    /// The `dedan.location` record: `"<lat>,<lon>"`.
    pub fn parse_record(s: &str) -> Option<Self> {
        let (lat, lon) = s.split_once(',')?;
        Some(Self::new(
            lat.trim().parse().ok()?,
            lon.trim().parse().ok()?,
        ))
    }
}

/// Bandwidth class: the per-account rate cap on a node and the admission unit. Ordered by capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Class {
    #[serde(rename = "100M")]
    M100,
    #[serde(rename = "1G")]
    G1,
}

impl Class {
    pub fn mbps(self) -> u32 {
        match self {
            Class::M100 => 100,
            Class::G1 => 1000,
        }
    }

    /// The `dedan.class` record.
    pub fn parse_record(s: &str) -> Option<Self> {
        match s.trim() {
            "100M" => Some(Class::M100),
            "1G" => Some(Class::G1),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Availability {
    #[serde(rename = "99")]
    P99,
    #[serde(rename = "99.9")]
    P999,
}

impl Availability {
    pub fn target(self) -> f64 {
        match self {
            Availability::P99 => 0.99,
            Availability::P999 => 0.999,
        }
    }
}

/// Node → all resolvers, every 10 s. The endpoint and the location are ENS records, not heartbeat fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub node_id: NodeId,
    pub classes: Vec<Class>,
    pub version: String,
    pub drain: bool,
    pub load_mbps: u32,
    pub connections: u32,
    pub cpu_pct: u8,
}

/// The `KeyRevealed` event of the registry: the key that minted an hour's fetch tokens, revealed by a signer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyReveal {
    pub node_id: NodeId,
    pub hour: u64,
    pub k_hour: B256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeType {
    Landing,
    Targeted,
    ThroughputClass,
    Conformance,
    Entry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    Dns,
    Connect,
    Tls,
    Timeout,
    Http5xx,
    WrongBytes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProbeMetrics {
    pub ttfb_ms: Option<u32>,
    pub hit_ttfb_ms: Option<u32>,
    pub rtt_ms: Option<u32>,
    /// Single-stream throughput in kbit/s: an integer, so a signed payload never carries a float that a JSON
    /// parser might round differently from the one that signed it.
    pub kbps: Option<u64>,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
    pub status: Option<u16>,
}

/// One probe as the auditor reports it. `node_id` is read from the token the node returned, so it is
/// absent on a failed fetch; `target` is the slot member the schedule aimed at, absent for landing probes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub probe: ProbeType,
    pub account: AccountId,
    pub asset: String,
    pub target: Option<NodeId>,
    pub node_id: Option<NodeId>,
    pub ok: bool,
    pub failure: Option<FailureClass>,
    pub metrics: ProbeMetrics,
    pub token: Option<String>,
    /// The client's /24 or /48 as the node saw it, what the token binds.
    pub prefix: String,
    pub ts: u64,
}

/// Auditor → aggregator, every 5 min.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeBatch {
    pub auditor_id: AuditorId,
    pub period: u64,
    pub results: Vec<ProbeResult>,
}

/// One round-trip measurement from the attester's own location cell to a node, as the app submits it to the
/// registry's `attest` and as the `Attested` event carries it (§7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    pub cell: Cell,
    pub node_id: NodeId,
    pub rtt_ms: u32,
    pub hit_ttfb_ms: Option<u32>,
    pub token: String,
    /// The attester's /24 or /48, what the token binds.
    pub prefix: String,
    /// Block time on chain.
    pub ts: u64,
    /// Made during an assigned probe: outweighs crowd rows in the map.
    pub anchor: bool,
}

/// What the app assembles before one `attest` transaction: the cell it is in and its members' round trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationBatch {
    pub auditor_id: AuditorId,
    pub cell: Cell,
    pub epoch: u64,
    pub attestations: Vec<Attestation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionReceipt {
    pub batch_hash: B256,
    pub period: u64,
    pub ts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertRequest {
    pub node_id: NodeId,
    pub account_id: AccountId,
    pub hostnames: Vec<String>,
    pub csr_pem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertIssued {
    pub chain_pem: String,
    pub not_after: u64,
    pub ari_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxtRecord {
    pub name: String,
    pub txt: String,
    pub expires: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub url: String,
    pub host_header: Option<String>,
    pub weight: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tier {
    pub nodes: u8,
    pub class: Class,
    pub availability: Availability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheRules {
    pub default_ttl_s: u32,
    pub keep_query_string: bool,
    pub max_object_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NodePolicy {
    pub operator_allowlist: Vec<Address>,
    pub operator_denylist: Vec<Address>,
    pub min_node_stake: u128,
}

/// The public account object (§9); origin secrets travel separately as [`OriginSecret`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConfig {
    pub account_id: AccountId,
    pub account_key: Address,
    pub domains: Vec<String>,
    pub origins: Vec<Origin>,
    pub tier: Tier,
    pub cache_rules: CacheRules,
    pub asset_list: String,
    pub node_policy: NodePolicy,
    pub version: u64,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    pub account_id: AccountId,
    pub operator_key: Address,
    pub scopes: Vec<String>,
    pub expiry: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginSecret {
    pub account_id: AccountId,
    pub node_id: NodeId,
    /// NaCl box to the node's x25519 key, base64.
    pub sealed_box: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurgeScope {
    Url,
    Prefix,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Purge {
    pub account_id: AccountId,
    pub seq: u64,
    pub scope: PurgeScope,
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetDigest {
    pub path: String,
    pub size: u64,
    pub sha256_current: String,
    pub sha256_previous: Option<String>,
    pub etag: Option<String>,
}

/// Control plane → auditors and aggregator, hourly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetSample {
    pub account_id: AccountId,
    pub hour: u64,
    pub assets: Vec<AssetDigest>,
}

/// What `publishRoot` receives, plus the two signatures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementBatch {
    pub period: u64,
    pub inputs_hash: B256,
    pub registry_block: u64,
    pub price_table_version: u64,
    pub payout_root: B256,
    pub bill_root: B256,
    pub total_cum_payout: u128,
    pub total_cum_bill: u128,
    pub sigs: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_bytes32_matches_solidity_literal() {
        let b = AccountId::new("acme").bytes32();
        assert_eq!(
            format!("{b}"),
            "0x61636d6500000000000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn node_id_is_the_label() {
        let n = NodeId::new("frankfurt");
        assert_eq!(n.signer(), "node:frankfurt");
        assert_eq!(n.name("dedan.eth"), "frankfurt.dedan.eth");
        assert_eq!(n.label_hash(), keccak256(b"frankfurt"));
        assert_eq!(serde_json::to_string(&n).unwrap(), "\"frankfurt\"");
    }

    #[test]
    fn records_parse() {
        assert_eq!(Class::parse_record("1G"), Some(Class::G1));
        assert_eq!(Class::parse_record("10G"), None);
        let p = LatLon::parse_record("41.0082,28.9784").unwrap();
        assert_eq!((p.lat, p.lon), (41.0082, 28.9784));
        assert!(LatLon::parse_record("nope").is_none());
    }

    #[test]
    fn messages_round_trip_json() {
        let hb = Heartbeat {
            node_id: NodeId::new("istanbul"),
            classes: vec![Class::M100, Class::G1],
            version: "0.1.0".into(),
            drain: false,
            load_mbps: 12,
            connections: 40,
            cpu_pct: 3,
        };
        let json = serde_json::to_string(&hb).unwrap();
        assert!(json.contains("\"100M\"") && json.contains("\"node_id\":\"istanbul\""));
        assert_eq!(serde_json::from_str::<Heartbeat>(&json).unwrap(), hb);

        let batch = AttestationBatch {
            auditor_id: AuditorId(Address::from([3u8; 20])),
            cell: Cell(0x85_1f_b4_67_ff_ff_ff_ff),
            epoch: 20_700,
            attestations: vec![Attestation {
                cell: Cell(0x85_1f_b4_67_ff_ff_ff_ff),
                node_id: NodeId::new("frankfurt"),
                rtt_ms: 12,
                hit_ttfb_ms: Some(20),
                token: "frankfurt.29000000.ab".into(),
                prefix: "10.77.0.0/24".into(),
                ts: 1_788_480_000,
                anchor: false,
            }],
        };
        let json = serde_json::to_string(&batch).unwrap();
        assert!(json.contains("\"auditor_id\":\"0x0303030303030303030303030303030303030303\""));
        assert_eq!(
            serde_json::from_str::<AttestationBatch>(&json).unwrap(),
            batch
        );
    }
}
