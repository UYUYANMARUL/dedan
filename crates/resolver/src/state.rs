//! Configuration, the shared snapshot and the sync loop that rebuilds it from the chain.

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::Address;
use alloy::providers::{Provider, RootProvider};
use dedan_core::assign::{assign, AccountInfo, AssignParams, Assignment, Yesterday};
use dedan_core::config::{self, AccountCfg, Deployment, Geo};
use dedan_core::ens::{EnsClient, NodeRecord};
use dedan_core::identity::Identity;
use dedan_core::map::{self, Map, MapParams, Validator};
use dedan_core::schedule;
use dedan_core::types::*;
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
    /// A node without a heartbeat for this long is not answered (3 missed 10 s beats); 0 disables the check.
    pub heartbeat_timeout: u64,
    /// The control plane's API: signed account configs replace the account file when set.
    pub cp_url: Option<String>,
    /// Challenge a name's endpoint before answering its record (§6); 0 disables, else the re-check interval.
    pub challenge_secs: u64,
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
            hosts: config::paths(&get("DEDAN_HOSTS", "/wan/hosts.txt")),
            zones_dir: get("DEDAN_ZONES_DIR", "/zones").into(),
            dns_addr: get("DEDAN_DNS_ADDR", "0.0.0.0:53").parse()?,
            http_addr: get("DEDAN_HTTP_ADDR", "0.0.0.0:8055").parse()?,
            sync_secs: get("DEDAN_SYNC_SECS", "10").parse()?,
            map_k: get("DEDAN_MAP_K", "1").parse()?,
            ns_ip: get("DEDAN_NS_IP", "10.77.0.53").parse()?,
            heartbeat_timeout: get("DEDAN_HEARTBEAT_TIMEOUT_SECS", "30").parse()?,
            cp_url: std::env::var("DEDAN_CP_URL").ok().filter(|s| !s.is_empty()),
            challenge_secs: get("DEDAN_CHALLENGE_SECS", "3600").parse()?,
        })
    }

    /// The zone with its trailing dot, lowercase.
    pub fn zone_dot(&self) -> String {
        format!("{}.", self.zone)
    }
}

impl State {
    pub fn accounts(&self) -> Vec<AccountInfo> {
        self.accounts.read().expect("accounts lock").clone()
    }

    /// The endpoint the name's record claims, once it proved itself (or the check is off).
    pub fn verified_ip(&self, node: &NodeId, record: Option<Ipv4Addr>) -> Option<Ipv4Addr> {
        let ip = record?;
        if self.cfg.challenge_secs == 0 {
            return Some(ip);
        }
        self.verified
            .read()
            .expect("verified lock")
            .get(node)
            .filter(|(v, _)| *v == ip)
            .map(|(v, _)| *v)
    }

    /// The control plane's configs, signed by its key, become the account list.
    async fn sync_accounts(&self, cp_url: &str, cp_key: Address) -> Result<(), Error> {
        let v: serde_json::Value = self
            .http
            .get(format!("{}/v1/configs", cp_url.trim_end_matches('/')))
            .send()
            .await?
            .json()
            .await?;
        let mut accounts = Vec::new();
        for c in v["configs"].as_array().cloned().unwrap_or_default() {
            let env: dedan_core::envelope::Envelope<AccountConfig> = serde_json::from_value(c)?;
            if env.signer != "cp" || env.recover()? != cp_key {
                tracing::warn!(account = %env.payload.account_id, "config not signed by the control plane; ignored");
                continue;
            }
            let p = env.payload;
            accounts.push(AccountInfo {
                id: p.account_id,
                nodes: p.tier.nodes,
                class: p.tier.class,
                availability: p.tier.availability,
                min_node_stake: p.node_policy.min_node_stake,
                operator_allowlist: p.node_policy.operator_allowlist,
                operator_denylist: p.node_policy.operator_denylist,
            });
        }
        *self.accounts.write().expect("accounts lock") = accounts;
        Ok(())
    }

    /// `GET http://<ip>:<port>/.well-known/dedan/challenge/<nonce>` must come back signed by a current signer
    /// key of the name (EIP-191 over the nonce). One endpoint per node; re-checked every `challenge_secs`.
    async fn challenge(&self, identity: &Identity, records: &BTreeMap<NodeId, NodeRecord>, now: u64) {
        for (label, rec) in records {
            let Some(ip) = rec.ipv4 else { continue };
            let fresh = self
                .verified
                .read()
                .expect("verified lock")
                .get(label)
                .is_some_and(|(v, at)| *v == ip && now.saturating_sub(*at) < self.cfg.challenge_secs);
            if fresh {
                continue;
            }
            let nonce = format!("{:x}", alloy::primitives::keccak256(format!("{label}:{ip}:{now}")));
            let url = format!("http://{ip}:{}/.well-known/dedan/challenge/{nonce}", rec.port);
            let ok = match self.http.get(&url).send().await {
                Ok(r) => match r.json::<serde_json::Value>().await {
                    Ok(v) => {
                        let sig = v["sig"].as_str().unwrap_or("");
                        let signed_nonce = v["nonce"].as_str().unwrap_or("");
                        let raw = hex::decode(sig.trim_start_matches("0x")).unwrap_or_default();
                        let signer = alloy::primitives::Signature::try_from(raw.as_slice())
                            .ok()
                            .and_then(|s| s.recover_address_from_msg(nonce.as_bytes()).ok());
                        signed_nonce == nonce
                            && v["label"].as_str() == Some(label.0.as_str())
                            && signer.is_some_and(|s| identity.signers(label).contains(&s))
                    }
                    Err(_) => false,
                },
                Err(_) => false,
            };
            let mut verified = self.verified.write().expect("verified lock");
            if ok {
                if !verified.get(label).is_some_and(|(v, _)| *v == ip) {
                    info!(node = %label, %ip, "endpoint challenge passed");
                }
                verified.insert(label.clone(), (ip, now));
            } else {
                if verified.remove(label).is_some() {
                    tracing::warn!(node = %label, %ip, "endpoint challenge failed; the record is not answered");
                }
            }
        }
    }

    /// Up = a heartbeat inside the timeout (or the check is off).
    pub async fn is_up(&self, node: &NodeId, now: u64) -> bool {
        if self.cfg.heartbeat_timeout == 0 {
            return true;
        }
        self.heartbeats
            .read()
            .await
            .get(node)
            .is_some_and(|(at, hb)| now.saturating_sub(*at) <= self.cfg.heartbeat_timeout && !hb.drain)
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
    /// The accounts: from the control plane's signed configs when it is configured, else the account file.
    pub accounts: std::sync::RwLock<Vec<AccountInfo>>,
    /// Endpoints that answered the challenge with a signature of a current signer key: node → (ip, when).
    pub verified: std::sync::RwLock<HashMap<NodeId, (Ipv4Addr, u64)>>,
    pub http: reqwest::Client,
    pub geo: std::sync::RwLock<Geo>,
    /// ACME TXT records published through the management API, by lowercase fqdn with trailing dot.
    pub txt: RwLock<HashMap<String, Vec<String>>>,
    /// The last heartbeat of each node (§5): when it arrived and what it said.
    pub heartbeats: RwLock<HashMap<NodeId, (u64, Heartbeat)>>,
    pub nonces: tokio::sync::Mutex<dedan_core::envelope::NonceCache>,
    pub snap: RwLock<Snapshot>,
    provider: RootProvider,
    /// Contract addresses from the deployment file, re-read on every refresh: the devnet starts the nameserver
    /// before the deployer has written the file, and a redeploy changes the addresses.
    chain: RwLock<Option<Arc<Chain>>>,
}

struct Chain {
    ens: EnsClient<RootProvider>,
    registry: Address,
    cells: Address,
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
        let accounts: Vec<AccountInfo> = AccountCfg::load_all(&cfg.accounts)
            .map(|a| a.iter().map(AccountCfg::info).collect())
            .unwrap_or_default();
        let geo = Geo::from_files(&cfg.hosts);
        let provider = RootProvider::new_http(cfg.rpc_url.parse()?);
        info!(
            accounts = accounts.len(),
            prefixes = geo.len(),
            "configured"
        );
        Ok(Self {
            cfg,
            accounts: std::sync::RwLock::new(accounts),
            verified: std::sync::RwLock::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()?,
            geo: std::sync::RwLock::new(geo),
            txt: RwLock::new(HashMap::new()),
            heartbeats: RwLock::new(HashMap::new()),
            nonces: tokio::sync::Mutex::new(dedan_core::envelope::NonceCache::default()),
            snap: RwLock::new(Snapshot::default()),
            provider,
            chain: RwLock::new(None),
        })
    }

    /// The contracts to sync from, and whether they differ from the previous refresh's (then the identity cache
    /// is stale and the replay starts over).
    async fn chain(&self) -> Result<(Arc<Chain>, bool), Error> {
        let dep = Deployment::load(&self.cfg.deployment)?;
        if let Some(c) = self.chain.read().await.as_ref() {
            if c.registry == dep.dedan_registry
                && c.cells == dep.dedan_cells
                && c.universal_resolver == dep.universal_resolver
            {
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
            cells: dep.dedan_cells,
            universal_resolver: dep.universal_resolver,
        });
        info!(registry = %dep.dedan_registry, cells = %dep.dedan_cells, universal_resolver = %dep.universal_resolver, "deployment loaded");
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
            .replay(&self.provider, &[chain.registry, chain.cells], head)
            .await?;
        let records = chain.ens.node_records(identity.nodes.keys()).await?;
        let now = unix_now();
        if let Some(cp) = &self.cfg.cp_url {
            if let Err(e) = self.sync_accounts(cp, Deployment::load(&self.cfg.deployment)?.control_plane).await {
                tracing::warn!(error = %e, "control plane configs unavailable; keeping the last account list");
            }
        }
        if self.cfg.challenge_secs > 0 {
            self.challenge(&identity, &records, now).await;
        }
        let params = MapParams {
            k_min: self.cfg.map_k,
            ..MapParams::v1()
        };
        let labels = identity.labels_by_hash();
        let geo = Geo::from_files(&self.cfg.hosts);
        // the contract's map, mirrored from its events on the chain's clock; the token and location checks
        // the contract cannot make, as evidence
        let map = Map::from_votes(
            &identity.attested,
            &identity.members,
            &labels,
            identity.block_time,
            &params,
        );
        let (_, map_evidence) = Validator::new(params.clone(), identity.reveals(), &geo)
            .validate_events(&identity.attested, &labels);
        *self.geo.write().expect("geo lock") = geo;
        let nodes = config::node_infos(&identity, &records);
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
        let seed = schedule::seed_of(now);
        let accounts = self.accounts();
        let assignment = assign(
            seed,
            &nodes,
            &accounts,
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
