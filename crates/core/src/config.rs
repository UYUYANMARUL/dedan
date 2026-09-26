//! What every service starts from: the deployment's contract addresses, the customer's account file (what
//! the customer's CLI turns into a config signed by its account key; services read the signed configs the
//! control plane relays, see [`crate::settlement`]), the client locator (GeoIP in production, the devnet's
//! host tables) and the node table the assignment reads.

use crate::assign::{AccountInfo, NodeInfo};
use crate::ens::NodeRecord;
use crate::identity::Identity;
use crate::map::GeoLocate;
use crate::token;
use crate::types::*;
use alloy::primitives::Address;
use alloy::signers::local::{coins_bip39::English, MnemonicBuilder, PrivateKeySigner};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

pub type ConfigError = Box<dyn std::error::Error + Send + Sync>;

/// `contracts/deployments/devnet.json`, as `Deploy.s.sol` writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub chain_id: u64,
    pub usdc: Address,
    pub dedan_registry: Address,
    pub dedan_cells: Address,
    pub universal_resolver: Address,
    pub settlement: Address,
    pub governance: Address,
    pub aggregator: Address,
    pub countersigner: Address,
    /// The control plane's key: the registrar that signs account registrations, and the ACME side.
    #[serde(default)]
    pub control_plane: Address,
}

impl Deployment {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }
}

/// One customer account as the customer describes it (§9): tier, hostnames, origin, asset list, cache rules,
/// node policy. The price is not here: the tier's price comes from the on-chain price table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountCfg {
    pub label: String,
    pub hostnames: Vec<String>,
    /// `S`, the node-count tier.
    pub nodes: u8,
    pub class: Class,
    pub availability: Availability,
    /// The origin the nodes fetch from and the digests are taken from, e.g. `http://origin`.
    #[serde(default)]
    pub origin: String,
    /// Path of the customer's asset manifest on the origin: one asset path per line.
    #[serde(default)]
    pub asset_list: String,
    #[serde(default)]
    pub cache_rules: Option<CacheRules>,
    #[serde(default)]
    pub node_policy: NodePolicy,
}

impl AccountCfg {
    pub fn load_all(path: impl AsRef<Path>) -> Result<Vec<Self>, ConfigError> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn id(&self) -> AccountId {
        AccountId::new(&self.label)
    }

    /// The config the customer signs with its account key (`dedan-cp config`), as version `version`.
    pub fn to_config(&self, version: u64) -> AccountConfig {
        let origin = self.origin.trim_end_matches('/');
        AccountConfig {
            account_id: self.id(),
            account_key: Address::ZERO,
            domains: self.hostnames.clone(),
            origins: vec![Origin {
                url: self.origin.clone(),
                host_header: None,
                weight: 1,
            }],
            tier: Tier {
                nodes: self.nodes,
                class: self.class,
                availability: self.availability,
            },
            cache_rules: self.cache_rules.clone().unwrap_or(CacheRules {
                default_ttl_s: 600,
                keep_query_string: true,
                max_object_bytes: 50 << 20,
            }),
            asset_list: if self.asset_list.is_empty() {
                String::new()
            } else {
                format!("{origin}{}", self.asset_list)
            },
            node_policy: self.node_policy.clone(),
            version,
            seq: 0,
        }
    }

    pub fn info(&self) -> AccountInfo {
        AccountInfo::from_config(&self.to_config(0))
    }
}

/// Where a client network is: the prefix of every `name city ip role lat lon` row maps to its coordinates.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Geo {
    pub by_prefix: HashMap<String, LatLon>,
}

impl Geo {
    pub fn from_hosts(text: &str) -> Self {
        let mut by_prefix = HashMap::new();
        for line in text.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 6 || f[0].starts_with('#') {
                continue;
            }
            if let (Ok(ip), Ok(lat), Ok(lon)) = (f[2].parse::<IpAddr>(), f[4].parse(), f[5].parse())
            {
                by_prefix.insert(token::prefix(ip), LatLon::new(lat, lon));
            }
        }
        Self { by_prefix }
    }

    /// Missing files are skipped, so a table for cities added while running can appear later.
    pub fn from_files(paths: &[PathBuf]) -> Self {
        let text: String = paths
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .collect::<Vec<_>>()
            .join("\n");
        Self::from_hosts(&text)
    }

    pub fn len(&self) -> usize {
        self.by_prefix.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_prefix.is_empty()
    }
}

impl GeoLocate for Geo {
    fn locate(&self, prefix: &str) -> Option<LatLon> {
        self.by_prefix.get(prefix).copied()
    }
}

/// A service's signing key from the environment: `DEDAN_KEY` (a 0x-hex private key) or, on the devnet,
/// `DEDAN_MNEMONIC` with `DEDAN_KEY_INDEX`.
pub fn signer_from_env() -> Result<PrivateKeySigner, ConfigError> {
    if let Ok(k) = std::env::var("DEDAN_KEY") {
        return Ok(k.parse()?);
    }
    let phrase =
        std::env::var("DEDAN_MNEMONIC").map_err(|_| "DEDAN_KEY or DEDAN_MNEMONIC is required")?;
    let index: u32 = std::env::var("DEDAN_KEY_INDEX")
        .unwrap_or_else(|_| "0".into())
        .parse()?;
    Ok(MnemonicBuilder::<English>::default()
        .phrase(phrase)
        .index(index)?
        .build()?)
}

/// Colon-separated paths from an environment variable such as `DEDAN_HOSTS`.
pub fn paths(list: &str) -> Vec<PathBuf> {
    list.split(':')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// The assignment's node table: identity from the registry cache, endpoint and class from the names' records.
/// A node without an IPv4 record is left out. ponytail: every node passes the entry audit until it exists.
pub fn node_infos(identity: &Identity, records: &BTreeMap<NodeId, NodeRecord>) -> Vec<NodeInfo> {
    identity
        .nodes
        .values()
        .filter_map(|n| {
            let r = records.get(&n.label)?;
            Some(NodeInfo {
                id: n.label.clone(),
                active: identity.node_active(&n.label),
                entry_passed: true,
                // ponytail: the declared `dedan.class` record would let an operator claim 1G and take ten times
                // the slots; everyone is 100M until class probes measure it (score's `class_qualified`)
                class: Class::M100,
                stake: n.stake,
                ip: IpAddr::V4(r.ipv4?),
                asn: None,
                owner: n.owner,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_rows_map_prefixes_to_points() {
        let g = Geo::from_hosts(
            "# comment\nfrankfurt frankfurt 10.77.16.1 node 50.1109 8.6821\nbad row\nuser-tokyo tokyo 10.77.32.30 user 35.6762 139.6503\n",
        );
        assert_eq!(g.len(), 2);
        assert_eq!(
            g.locate("10.77.32.0/24"),
            Some(LatLon::new(35.6762, 139.6503))
        );
        assert_eq!(g.locate("10.77.33.0/24"), None);
    }

    #[test]
    fn account_file_becomes_a_config() {
        let a: AccountCfg = serde_json::from_str(
            r#"{"label":"acme","hostnames":["www.images.com"],"nodes":6,"class":"100M","availability":"99.9","origin":"http://origin/","asset_list":"/assets/manifest.txt","cache_rules":{"default_ttl_s":60,"keep_query_string":true,"max_object_bytes":1048576},"price_usdc_per_day":7200}"#,
        )
        .unwrap();
        let c = a.to_config(7);
        assert_eq!(c.version, 7);
        assert_eq!(c.asset_list, "http://origin/assets/manifest.txt");
        assert_eq!(c.cache_rules.default_ttl_s, 60);
        assert_eq!(a.info().nodes, 6);
    }
}
