//! Configuration, the shared snapshot and the sync loop that rebuilds it from the chain.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
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
use dedan_core::settlement;
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
    /// A node without a heartbeat for this long is not answered (3 missed 10 s beats); 0 disables the check.
    pub heartbeat_timeout: u64,
    /// The control plane's API: signed account configs replace the account file when set.
    pub cp_url: Option<String>,
    /// Challenge a name's endpoint before answering its record (§6); 0 disables, else the re-check interval.
    pub challenge_secs: u64,
    /// The settlement period, to price a period of service (86,400 in production, 120 on the devnet).
    pub period_secs: u64,
    /// An account whose effective balance cannot cover this many periods at list price is not answered (§8).
    pub balance_periods: u64,
    /// The on-chain price table version the balance check prices tiers with.
    pub price_version: u32,
    /// The aggregator's API: the previous period's published sets, quality and bills, so the resolver's slot
    /// sets are the ones the aggregator pays for, and unapplied bills count against balances.
    pub aggregator_url: Option<String>,
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
            map_k: get("DEDAN_MAP_K", "3").parse()?,
            ns_ip: get("DEDAN_NS_IP", "10.77.0.53").parse()?,
            heartbeat_timeout: get("DEDAN_HEARTBEAT_TIMEOUT_SECS", "30").parse()?,
            cp_url: std::env::var("DEDAN_CP_URL").ok().filter(|s| !s.is_empty()),
            challenge_secs: get("DEDAN_CHALLENGE_SECS", "3600").parse()?,
            period_secs: get("DEDAN_PERIOD_SECS", "86400").parse()?,
            balance_periods: get("DEDAN_BALANCE_PERIODS", "4").parse()?,
            price_version: get("DEDAN_PRICE_VERSION", "1").parse()?,
            aggregator_url: std::env::var("DEDAN_AGGREGATOR_URL").ok().filter(|s| !s.is_empty()),
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

    /// The endpoint the name's record claims, once it proved itself (or the check is off) and no other name
    /// proved the same address: one box answering for two names is served for neither (§6).
    pub fn verified_ip(&self, node: &NodeId, record: Option<Ipv4Addr>) -> Option<Ipv4Addr> {
        let ip = record?;
        self.verified_addr(node, IpAddr::V4(ip)).then_some(ip)
    }

    /// The same for the name's IPv6 record.
    pub fn verified_ip6(&self, node: &NodeId, record: Option<Ipv6Addr>) -> Option<Ipv6Addr> {
        let ip = record?;
        self.verified_addr(node, IpAddr::V6(ip)).then_some(ip)
    }

    fn verified_addr(&self, node: &NodeId, ip: IpAddr) -> bool {
        if self.cfg.challenge_secs == 0 {
            return true;
        }
        !self.conflicts.read().expect("conflicts lock").contains(&ip)
            && self
                .verified
                .read()
                .expect("verified lock")
                .get(node)
                .is_some_and(|m| m.contains_key(&ip))
    }

    /// An account whose effective balance cannot cover `balance_periods` periods at list price (§8).
    pub fn is_suspended(&self, account: &AccountId) -> bool {
        self.suspended.read().expect("suspended lock").contains(account)
    }

    /// The configs the control plane relays count only with the signature of the account key on chain (§9),
    /// and only when newer than the one held: the control plane can relay a config but cannot write or replay
    /// one.
    async fn sync_accounts(&self, cp_url: &str, settlement_addr: Address) -> Result<(), Error> {
        let v: serde_json::Value = self
            .http
            .get(format!("{}/v1/configs", cp_url.trim_end_matches('/')))
            .send()
            .await?
            .json()
            .await?;
        let mut configs = self.configs.read().expect("configs lock").clone();
        for c in v["configs"].as_array().cloned().unwrap_or_default() {
            let env: dedan_core::envelope::Envelope<AccountConfig> = serde_json::from_value(c)?;
            let id = env.payload.account_id.clone();
            let held = configs.get(&id).map(|c| c.version);
            if !settlement::newer(held, env.payload.version) {
                if held.is_some_and(|h| env.payload.version < h) {
                    tracing::warn!(account = %id, held, offered = env.payload.version, "older config refused");
                }
                continue;
            }
            let key = settlement::account(&self.provider, settlement_addr, &id, None).await?.key;
            match settlement::verify_config(&env, key) {
                Ok(()) => {
                    info!(account = %id, version = env.payload.version, "config accepted");
                    configs.insert(id, env.payload);
                }
                Err(why) => tracing::warn!(account = %id, why, "config refused"),
            }
        }
        *self.accounts.write().expect("accounts lock") = configs.values().map(AccountInfo::from_config).collect();
        *self.configs.write().expect("configs lock") = configs;
        Ok(())
    }

    /// The previous period as the aggregator published it: its sets and quality make today's sets sticky the
    /// way the aggregator computes them, its cumulative bills are the published-but-unapplied part of balances.
    async fn sync_previous(&self, url: &str) -> Result<(), Error> {
        let p: Previous = self
            .http
            .get(format!("{}/v1/previous", url.trim_end_matches('/')))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        *self.previous.write().await = Some(p);
        Ok(())
    }

    /// Suspends the accounts whose effective balance (on-chain balance, minus bills published but not
    /// applied, minus a pending withdrawal) cannot cover `balance_periods` periods at their tier's list price.
    /// ponytail: two contract reads per account per refresh; a multicall when accounts number in the hundreds
    async fn check_balances(&self, settlement_addr: Address) -> Result<(), Error> {
        let configs = self.configs.read().expect("configs lock").clone();
        let bills = self
            .previous
            .read()
            .await
            .as_ref()
            .map(|p| p.bills.clone())
            .unwrap_or_default();
        let mut suspended = BTreeSet::new();
        for (id, c) in &configs {
            let state = settlement::account(&self.provider, settlement_addr, id, None).await?;
            let per_day =
                settlement::price(&self.provider, settlement_addr, self.cfg.price_version, &c.tier, None).await?;
            let need = settlement::list_price(per_day, self.cfg.period_secs) * self.cfg.balance_periods as u128;
            let effective = state.effective_balance(bills.get(id).copied().unwrap_or(0));
            if effective < need || per_day == 0 {
                suspended.insert(id.clone());
            }
        }
        let mut held = self.suspended.write().expect("suspended lock");
        for id in suspended.difference(&held) {
            tracing::warn!(account = %id, "effective balance below the minimum: the account's names are not answered");
        }
        for id in held.difference(&suspended) {
            info!(account = %id, "balance restored: the account's names are answered again");
        }
        *held = suspended;
        Ok(())
    }

    /// `GET http://<ip>:<port>/.well-known/dedan/challenge/<nonce>` must come back signed by a current signer
    /// key of the name (EIP-191 over the nonce). One endpoint per node; re-checked every `challenge_secs`.
    async fn challenge(&self, identity: &Identity, records: &BTreeMap<NodeId, NodeRecord>, now: u64) {
        // an address a record no longer names is forgotten
        self.verified.write().expect("verified lock").retain(|label, m| {
            let rec = records.get(label);
            m.retain(|ip, _| match ip {
                IpAddr::V4(v) => rec.and_then(|r| r.ipv4) == Some(*v),
                IpAddr::V6(v) => rec.and_then(|r| r.ipv6) == Some(*v),
            });
            !m.is_empty()
        });
        for (label, rec) in records {
            let addrs = [rec.ipv4.map(IpAddr::V4), rec.ipv6.map(IpAddr::V6)];
            for ip in addrs.into_iter().flatten() {
                self.challenge_one(identity, label, ip, rec.port, now).await;
            }
        }
        let verified = self.verified.read().expect("verified lock").clone();
        let conflicts = conflicted(&verified);
        let mut held = self.conflicts.write().expect("conflicts lock");
        for ip in conflicts.difference(&held) {
            let names: Vec<&NodeId> = verified.iter().filter(|(_, m)| m.contains_key(ip)).map(|(n, _)| n).collect();
            tracing::warn!(%ip, ?names, "one endpoint proved for several names: none of them is answered");
        }
        *held = conflicts;
    }

    /// One endpoint of one name: `GET http://<ip>:<port>/.well-known/dedan/challenge/<nonce>`.
    async fn challenge_one(&self, identity: &Identity, label: &NodeId, ip: IpAddr, port: u16, now: u64) {
        {
            let fresh = self
                .verified
                .read()
                .expect("verified lock")
                .get(label)
                .and_then(|m| m.get(&ip))
                .is_some_and(|at| now.saturating_sub(*at) < self.cfg.challenge_secs);
            if fresh {
                return;
            }
            let nonce = format!("{:x}", alloy::primitives::keccak256(format!("{label}:{ip}:{now}")));
            let host = match ip {
                IpAddr::V4(v) => v.to_string(),
                IpAddr::V6(v) => format!("[{v}]"),
            };
            let url = format!("http://{host}:{port}/.well-known/dedan/challenge/{nonce}");
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
            let m = verified.entry(label.clone()).or_default();
            if ok {
                if m.insert(ip, now).is_none() {
                    info!(node = %label, %ip, "endpoint challenge passed");
                }
            } else if m.remove(&ip).is_some() {
                tracing::warn!(node = %label, %ip, "endpoint challenge failed; the record is not answered");
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

/// One address proved by two names is the same box answering for both: served for neither (§6).
pub fn conflicted(verified: &HashMap<NodeId, HashMap<IpAddr, u64>>) -> HashSet<IpAddr> {
    let mut count: HashMap<IpAddr, usize> = HashMap::new();
    for m in verified.values() {
        for ip in m.keys() {
            *count.entry(*ip).or_default() += 1;
        }
    }
    count.into_iter().filter(|(_, n)| *n > 1).map(|(ip, _)| ip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_two_names_proved_is_nobodys() {
        let a: IpAddr = "10.77.16.1".parse().unwrap();
        let b: IpAddr = "10.77.32.1".parse().unwrap();
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        let verified = HashMap::from([
            (NodeId::new("fra"), HashMap::from([(a, 1), (v6, 1)])),
            (NodeId::new("fra2"), HashMap::from([(a, 2)])),
            (NodeId::new("tok"), HashMap::from([(b, 1)])),
        ]);
        assert_eq!(conflicted(&verified), HashSet::from([a]));
    }
}

/// `GET /v1/previous` of the aggregator: the latest published period's sets, quality and cumulative bills.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Previous {
    pub period: u64,
    pub assignment: Assignment,
    pub quality: Vec<(AccountId, NodeId, f64)>,
    pub bills: BTreeMap<AccountId, u128>,
}

pub struct State {
    pub cfg: Config,
    /// The accounts: from the account-signed configs the control plane relays when it is configured, else the
    /// account file.
    pub accounts: std::sync::RwLock<Vec<AccountInfo>>,
    /// The verified configs themselves, newest version per account.
    pub configs: std::sync::RwLock<BTreeMap<AccountId, AccountConfig>>,
    /// Accounts under the balance minimum: their names are not answered.
    pub suspended: std::sync::RwLock<BTreeSet<AccountId>>,
    /// Endpoints that answered the challenge with a signature of a current signer key: node → ip → when.
    pub verified: std::sync::RwLock<HashMap<NodeId, HashMap<IpAddr, u64>>>,
    /// Addresses more than one name proved: answered for none.
    pub conflicts: std::sync::RwLock<HashSet<IpAddr>>,
    pub previous: RwLock<Option<Previous>>,
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
    settlement: Address,
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
            configs: std::sync::RwLock::new(BTreeMap::new()),
            suspended: std::sync::RwLock::new(BTreeSet::new()),
            verified: std::sync::RwLock::new(HashMap::new()),
            conflicts: std::sync::RwLock::new(HashSet::new()),
            previous: RwLock::new(None),
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
                && c.settlement == dep.settlement
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
            settlement: dep.settlement,
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
        let mut identity = self.snap.read().await.identity.clone();
        if changed {
            identity = Identity::default();
        }
        let applied = identity
            .replay(&self.provider, &[chain.registry, chain.cells], head)
            .await?;
        let records = chain.ens.node_records(identity.nodes.keys()).await?;
        let now = unix_now();
        if let Some(cp) = &self.cfg.cp_url {
            if let Err(e) = self.sync_accounts(cp, chain.settlement).await {
                tracing::warn!(error = %e, "control plane configs unavailable; keeping the last account list");
            }
            if let Err(e) = self.check_balances(chain.settlement).await {
                tracing::warn!(error = %e, "balances unreadable; keeping the last suspensions");
            }
        }
        if let Some(url) = &self.cfg.aggregator_url {
            if let Err(e) = self.sync_previous(url).await {
                tracing::debug!(error = %e, "no published period from the aggregator yet");
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
        // sticky on the period the aggregator published, the one it pays for; with no aggregator (or before its
        // first period) nobody has a previous period, auditors included, so the sets still agree
        let previous = self.previous.read().await.clone();
        let quality: HashMap<(AccountId, NodeId), f64> = previous
            .iter()
            .flat_map(|p| p.quality.iter())
            .map(|(a, n, q)| ((a.clone(), n.clone()), *q))
            .collect();
        let yesterday = previous.as_ref().map(|p| Yesterday {
            assignment: &p.assignment,
            quality: &quality,
        });
        let seed = schedule::seed_of(now);
        let accounts = self.accounts();
        let assignment = assign(
            seed,
            &nodes,
            &accounts,
            &map,
            yesterday.as_ref(),
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
