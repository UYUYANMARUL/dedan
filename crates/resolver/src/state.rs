//! Configuration, the shared snapshot and the sync loop that rebuilds it from the chain.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{keccak256, Address};
use alloy::providers::{Provider, RootProvider};
use dedan_core::assign::{assign, AccountInfo, AssignParams, Assignment, NodeInfo, Yesterday};
use dedan_core::ens::{EnsClient, NodeRecord};
use dedan_core::identity::Identity;
use dedan_core::map::{self, GeoLocate, Map, MapParams};
use dedan_core::token;
use dedan_core::types::*;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::info;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone)]
pub struct Config {
    pub rpc_url: String,
    pub deployment: PathBuf,
    pub zone: String,
    pub parent: String,
    pub accounts: PathBuf,
    /// Host tables (`name city ip role lat lon`), colon-separated; missing files are skipped and every file
    /// is re-read on each refresh, so cities added while running (deploy/scripts/add-node.sh) are known.
    pub hosts: Vec<PathBuf>,
    pub zones_dir: PathBuf,
    pub dns_addr: SocketAddr,
    pub http_addr: SocketAddr,
    pub sync_secs: u64,
    pub map_k: usize,
    pub ns_ip: Ipv4Addr,
}

impl Config {
    pub fn from_env() -> Result<Self, Error> {
        let get = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
        Ok(Self {
            rpc_url: get("DEDAN_RPC_URL", "http://anvil:8545"),
            deployment: get("DEDAN_DEPLOYMENT", "/deployments/devnet.json").into(),
            zone: get("DEDAN_ZONE", "dedan-cdn.test")
                .trim_end_matches('.')
                .to_lowercase(),
            parent: get("DEDAN_PARENT", "dedan.eth"),
            accounts: get("DEDAN_ACCOUNTS", "/devnet/accounts.json").into(),
            hosts: get("DEDAN_HOSTS", "/wan/hosts.txt")
                .split(':')
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect(),
            zones_dir: get("DEDAN_ZONES_DIR", "/zones").into(),
            dns_addr: get("DEDAN_DNS_ADDR", "0.0.0.0:53").parse()?,
            http_addr: get("DEDAN_HTTP_ADDR", "0.0.0.0:8055").parse()?,
            sync_secs: get("DEDAN_SYNC_SECS", "10").parse()?,
            map_k: get("DEDAN_MAP_K", "1").parse()?,
            ns_ip: get("DEDAN_NS_IP", "10.77.0.53").parse()?,
        })
    }

    /// The zone with its trailing dot, lowercase.
    pub fn zone_dot(&self) -> String {
        format!("{}.", self.zone)
    }
}

#[derive(Debug, Deserialize)]
struct Deployment {
    #[serde(rename = "dedanRegistry")]
    dedan_registry: Address,
    #[serde(rename = "universalResolver")]
    universal_resolver: Address,
}

#[derive(Debug, Deserialize)]
struct AccountCfg {
    label: String,
    /// Informational on the devnet: the customer's own zone files carry the CNAMEs.
    #[allow(dead_code)]
    hostnames: Vec<String>,
    nodes: u8,
    class: Class,
    availability: Availability,
}

/// Where a client network is, from the devnet's host table (GeoIP in production).
#[derive(Debug, Default)]
pub struct Geo {
    by_prefix: HashMap<String, LatLon>,
}

impl Geo {
    /// `name city ip role lat lon` rows; the prefix of each ip maps to its coordinates.
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
}

impl GeoLocate for Geo {
    fn locate(&self, prefix: &str) -> Option<LatLon> {
        self.by_prefix.get(prefix).copied()
    }
}

/// Everything a query needs, rebuilt by the sync loop and swapped in whole.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub identity: Identity,
    pub records: BTreeMap<NodeId, NodeRecord>,
    pub map: Map,
    pub map_evidence: Vec<map::Evidence>,
    pub assignment: Assignment,
    pub updated_at: u64,
    pub block: u64,
}

pub struct State {
    pub cfg: Config,
    pub accounts: Vec<AccountInfo>,
    pub geo: std::sync::RwLock<Geo>,
    /// ACME TXT records published through the management API, by lowercase fqdn with trailing dot.
    pub txt: RwLock<HashMap<String, Vec<String>>>,
    pub snap: RwLock<Snapshot>,
    provider: RootProvider,
    /// Contract addresses from the deployment file, re-read on every refresh: the devnet starts the nameserver
    /// before the deployer has written the file, and a redeploy changes the addresses.
    chain: RwLock<Option<Arc<Chain>>>,
}

struct Chain {
    ens: EnsClient<RootProvider>,
    registry: Address,
    universal_resolver: Address,
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl State {
    pub async fn new(cfg: Config) -> Result<Self, Error> {
        let accounts_cfg: Vec<AccountCfg> =
            serde_json::from_str(&std::fs::read_to_string(&cfg.accounts)?)?;
        let accounts: Vec<AccountInfo> = accounts_cfg
            .iter()
            .map(|a| AccountInfo {
                id: AccountId::new(&a.label),
                nodes: a.nodes,
                class: a.class,
                availability: a.availability,
                min_node_stake: 0,
                operator_allowlist: vec![],
                operator_denylist: vec![],
            })
            .collect();
        let geo = Geo::from_files(&cfg.hosts);
        let provider = RootProvider::new_http(cfg.rpc_url.parse()?);
        info!(
            accounts = accounts.len(),
            prefixes = geo.len(),
            "configured"
        );
        Ok(Self {
            cfg,
            accounts,
            geo: std::sync::RwLock::new(geo),
            txt: RwLock::new(HashMap::new()),
            snap: RwLock::new(Snapshot::default()),
            provider,
            chain: RwLock::new(None),
        })
    }

    /// The contracts to sync from, and whether they differ from the previous refresh's (then the identity cache
    /// is stale and the replay starts over).
    async fn chain(&self) -> Result<(Arc<Chain>, bool), Error> {
        let dep: Deployment =
            serde_json::from_str(&std::fs::read_to_string(&self.cfg.deployment)?)?;
        if let Some(c) = self.chain.read().await.as_ref() {
            if c.registry == dep.dedan_registry && c.universal_resolver == dep.universal_resolver {
                return Ok((c.clone(), false));
            }
        }
        let chain = Arc::new(Chain {
            ens: EnsClient::new(
                self.provider.clone(),
                dep.universal_resolver,
                self.cfg.parent.clone(),
            ),
            registry: dep.dedan_registry,
            universal_resolver: dep.universal_resolver,
        });
        info!(registry = %dep.dedan_registry, universal_resolver = %dep.universal_resolver, "deployment loaded");
        *self.chain.write().await = Some(chain.clone());
        Ok((chain, true))
    }

    /// Replays new registry events, re-reads every node's ENS records, rebuilds the map from the chain's
    /// attestations and recomputes the assignment; then swaps the snapshot.
    /// ponytail: every node's records are re-read on every tick; watch the resolver's events once the
    /// node count makes that visible
    pub async fn refresh(&self) -> Result<(), Error> {
        let (chain, changed) = self.chain().await?;
        let head = self.provider.get_block_number().await?;
        let (mut identity, prev) = {
            let s = self.snap.read().await;
            (s.identity.clone(), s.assignment.clone())
        };
        if changed {
            identity = Identity::default();
        }
        let applied = identity
            .replay(&self.provider, chain.registry, head)
            .await?;
        let mut records = BTreeMap::new();
        for label in identity.nodes.keys() {
            records.insert(label.clone(), chain.ens.node_record(label).await?);
        }
        let now = unix_now();
        let params = MapParams {
            k_min: self.cfg.map_k,
            ..MapParams::v1()
        };
        let labels = identity.labels_by_hash();
        let geo = Geo::from_files(&self.cfg.hosts);
        let (map, map_evidence) = Map::from_chain(
            &identity.attested,
            identity.reveals(),
            &labels,
            &geo,
            now,
            &params,
        );
        *self.geo.write().expect("geo lock") = geo;
        let nodes: Vec<NodeInfo> = identity
            .nodes
            .values()
            .filter_map(|n| {
                let r = records.get(&n.label)?;
                Some(NodeInfo {
                    id: n.label.clone(),
                    active: identity.node_active(&n.label),
                    entry_passed: true,
                    class: r.class.unwrap_or(Class::M100),
                    stake: n.stake,
                    ip: IpAddr::V4(r.ipv4?),
                    asn: None,
                    owner: n.owner,
                })
            })
            .collect();
        // yesterday's members count as fully served until the scoring loop exists
        let quality: HashMap<(AccountId, NodeId), f64> = prev
            .sets
            .iter()
            .flat_map(|(a, set)| set.iter().map(move |n| ((a.clone(), n.clone()), 1.0)))
            .collect();
        let yesterday = Yesterday {
            assignment: &prev,
            quality: &quality,
        };
        let seed = keccak256((now / 86_400).to_be_bytes());
        let assignment = assign(
            seed,
            &nodes,
            &self.accounts,
            &map,
            Some(&yesterday),
            &AssignParams::default(),
        );
        if applied > 0 {
            info!(
                block = head,
                events = applied,
                nodes = nodes.len(),
                attestations = identity.attested.len(),
                covered = map.covered.len(),
                "snapshot rebuilt"
            );
        }
        *self.snap.write().await = Snapshot {
            identity,
            records,
            map,
            map_evidence,
            assignment,
            updated_at: now,
            block: head,
        };
        Ok(())
    }
}
