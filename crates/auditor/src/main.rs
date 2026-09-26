//! `dedan-auditor`: the auditor app (ARCHITECTURE.md §7). One World ID-verified human runs it from wherever
//! it is. It measures round trips to every node from its own H3 cell and attests them to the Dedan registry,
//! runs the assigned probes of the schedule against each account's slot members and ships the signed results
//! to the aggregator every few minutes. It attests only the members of its own cell, one batch a minute at most,
//! each sample its newest vote for that node (`DedanCells`). Its view of the network is the chain's: names and
//! records from ENS, membership, the map and the assignment recomputed from the events with `dedan-core`,
//! never a resolver's word.

mod chain;
mod probe;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::Address;
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use dedan_core::assign::{assign, AccountInfo, AssignParams, Assignment, Yesterday};
use dedan_core::config::{self, AccountCfg, Deployment};
use dedan_core::ens::{EnsClient, NodeRecord};
use dedan_core::envelope::Envelope;
use dedan_core::identity::Identity;
use dedan_core::map::{Map, MapParams};
use dedan_core::schedule;
use dedan_core::settlement;
use dedan_core::token;
use dedan_core::types::*;
use dedan_core::geo;
use sha2::Digest;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

type Error = Box<dyn std::error::Error + Send + Sync>;

struct Cfg {
    rpc_url: String,
    deployment: PathBuf,
    /// The account file, used only when no control plane is configured.
    accounts: PathBuf,
    /// The control plane: account-signed configs, verified against the keys on chain.
    cp_url: Option<String>,
    parent: String,
    aggregator: String,
    /// How many auditors the schedule assigns to each account (§7: at least 3, target 5).
    auditors_per_account: usize,
    /// Where this human is; without it the app probes but never attests.
    location: Option<LatLon>,
    period_secs: u64,
    sync_secs: u64,
    attest_secs: u64,
    batch_secs: u64,
    probe_mean_s: f64,
    sample_n: usize,
    map_k: usize,
    probes: bool,
    /// Devnet only: report every probe as a success with a made-up token, never fetching anything.
    lie: bool,
    /// An address on the network the nodes see this app from; a UDP "connect" to it names the interface.
    route_probe: IpAddr,
}

impl Cfg {
    fn from_env() -> Result<Self, Error> {
        let get = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
        Ok(Self {
            rpc_url: get("DEDAN_RPC_URL", "http://anvil:8545"),
            deployment: get("DEDAN_DEPLOYMENT", "/deployments/devnet.json").into(),
            accounts: get("DEDAN_ACCOUNTS", "/devnet/accounts.json").into(),
            cp_url: std::env::var("DEDAN_CP_URL").ok().filter(|s| !s.is_empty()),
            parent: get("DEDAN_PARENT", "dedan.eth"),
            aggregator: get("DEDAN_AGGREGATOR", "http://aggregator:8060"),
            auditors_per_account: get("DEDAN_AUDITORS_PER_ACCOUNT", "5").parse()?,
            location: std::env::var("DEDAN_LOCATION")
                .ok()
                .and_then(|s| LatLon::parse_record(&s)),
            period_secs: get("DEDAN_PERIOD_SECS", "86400").parse()?,
            sync_secs: get("DEDAN_SYNC_SECS", "10").parse()?,
            attest_secs: get("DEDAN_ATTEST_SECS", "600").parse()?,
            batch_secs: get("DEDAN_BATCH_SECS", "300").parse()?,
            probe_mean_s: get("DEDAN_PROBE_MEAN_SECS", "180").parse()?,
            sample_n: get("DEDAN_SAMPLE_N", "4").parse()?,
            map_k: get("DEDAN_MAP_K", "3").parse()?,
            probes: get("DEDAN_PROBES", "1") != "0",
            lie: get("DEDAN_DEVNET_LIE", "0") == "1",
            route_probe: get("DEDAN_ROUTE_PROBE", "10.77.0.53").parse()?,
        })
    }
}

/// What the chain says right now, rebuilt every few seconds.
#[derive(Default, Clone)]
struct Table {
    identity: Identity,
    records: BTreeMap<NodeId, NodeRecord>,
    assignment: Assignment,
    /// Today's sampled assets per account.
    sample: BTreeMap<AccountId, Vec<String>>,
    /// The accounts: account-signed configs, newest version each.
    configs: BTreeMap<AccountId, AccountConfig>,
    /// The accounts the schedule assigned to this auditor today.
    mine: BTreeSet<AccountId>,
}

/// `GET /v1/previous` of the aggregator: the published period the aggregator's sets are sticky on.
#[derive(Default, Clone, serde::Deserialize)]
struct Previous {
    assignment: Assignment,
    quality: Vec<(AccountId, NodeId, f64)>,
}

struct App {
    cfg: Cfg,
    dep: Deployment,
    key: PrivateKeySigner,
    me: Address,
    prefix: String,
    provider: DynProvider,
    table: RwLock<Table>,
    results: Mutex<Vec<ProbeResult>>,
    nonce: AtomicU64,
    http: reqwest::Client,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The address this app has on the network the nodes see it from: a UDP socket "connected" toward it
/// picks the interface without sending anything.
fn local_ip(toward: IpAddr) -> Result<IpAddr, Error> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0")?;
    s.connect(SocketAddr::new(toward, 9))?;
    Ok(s.local_addr()?.ip())
}

/// Devnet only: a well-formed token with a MAC nobody minted.
fn forged_token(node: &NodeId, prefix: &str) -> String {
    token::Token {
        node_id: node.clone(),
        minute: token::minute_of(unix_now()),
        prefix: prefix.to_owned(),
        mac: [0u8; 32],
    }
    .to_string()
}

/// The network the node saw this app in, from inside its token; this app's own view when there is none (an
/// auditor behind NAT would otherwise report a network the node never saw, and every token would fail).
fn seen_prefix(tok: Option<&str>, own: &str) -> String {
    tok.and_then(|t| t.parse::<token::Token>().ok())
        .map(|t| t.prefix)
        .unwrap_or_else(|| own.to_owned())
}

/// A fetch of an asset from the customer's own list succeeded only with a 2xx (or a 304): a 421 or a 404 from
/// a node is a failure, attributed like a 5xx (the node's only when another member served the asset).
fn failure_of(status: u16) -> Option<FailureClass> {
    match status {
        200..=299 | 304 => None,
        500..=599 => Some(FailureClass::Http5xx),
        _ => Some(FailureClass::Http4xx),
    }
}

impl App {
    async fn hostname(&self, id: &AccountId) -> String {
        self.table
            .read()
            .await
            .configs
            .get(id)
            .and_then(|a| a.domains.first().cloned())
            .unwrap_or_else(|| format!("{id}.dedan-cdn.test"))
    }

    /// The accounts: from the control plane when configured (each config verified against the account key on
    /// chain, newest version kept), else the account file.
    async fn configs(&self, held: &BTreeMap<AccountId, AccountConfig>) -> Result<BTreeMap<AccountId, AccountConfig>, Error> {
        let Some(cp) = &self.cfg.cp_url else {
            return Ok(AccountCfg::load_all(&self.cfg.accounts)?
                .iter()
                .map(|a| (a.id(), a.to_config(1)))
                .collect());
        };
        let v: serde_json::Value = self
            .http
            .get(format!("{}/v1/configs", cp.trim_end_matches('/')))
            .send()
            .await?
            .json()
            .await?;
        let mut out = held.clone();
        for c in v["configs"].as_array().cloned().unwrap_or_default() {
            let env: Envelope<AccountConfig> = serde_json::from_value(c)?;
            let id = env.payload.account_id.clone();
            if !settlement::newer(out.get(&id).map(|c| c.version), env.payload.version) {
                continue;
            }
            let key = settlement::account(&self.provider, self.dep.settlement, &id, None).await?.key;
            match settlement::verify_config(&env, key) {
                Ok(()) => {
                    out.insert(id, env.payload);
                }
                Err(why) => warn!(account = %id, why, "config refused"),
            }
        }
        Ok(out)
    }

    async fn previous(&self) -> Option<Previous> {
        let r = self
            .http
            .get(format!("{}/v1/previous", self.cfg.aggregator))
            .send()
            .await
            .ok()?;
        if !r.status().is_success() {
            return None;
        }
        r.json().await.ok()
    }

    /// Registry events, records, map, assignment and today's asset samples, from the chain and the origins.
    async fn sync(&self, identity: &mut Identity, ens: &EnsClient<DynProvider>) -> Result<(), Error> {
        let head = self.provider.get_block_number().await?;
        identity
            .replay(&self.provider, &[self.dep.dedan_registry, self.dep.dedan_cells], head)
            .await?;
        let records = ens.node_records(identity.nodes.keys()).await?;
        let now = unix_now();
        let params = MapParams {
            k_min: self.cfg.map_k,
            ..MapParams::v1()
        };
        let map = Map::from_votes(
            &identity.attested,
            &identity.members,
            &identity.labels_by_hash(),
            identity.block_time,
            &params,
        );
        let nodes = config::node_infos(identity, &records);
        let held = self.table.read().await.configs.clone();
        let configs = match self.configs(&held).await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "account configs unavailable; keeping the last ones");
                held
            }
        };
        let infos: Vec<AccountInfo> = configs.values().map(AccountInfo::from_config).collect();
        let seed = schedule::seed_of(now);
        // the same sets the resolvers answer and the aggregator pays: sticky on the aggregator's published period
        let previous = self.previous().await;
        let quality: HashMap<(AccountId, NodeId), f64> = previous
            .iter()
            .flat_map(|p| p.quality.iter())
            .map(|(a, n, q)| ((a.clone(), n.clone()), *q))
            .collect();
        let yesterday = previous.as_ref().map(|p| Yesterday {
            assignment: &p.assignment,
            quality: &quality,
        });
        let assignment = assign(seed, &nodes, &infos, &map, yesterday.as_ref(), &AssignParams::default());
        let candidates = identity.auditors();
        let mine: BTreeSet<AccountId> = configs
            .keys()
            .filter(|id| {
                schedule::auditors_for(seed, id, &candidates, self.cfg.auditors_per_account).contains(&self.me)
            })
            .cloned()
            .collect();
        let mut sample = BTreeMap::new();
        for a in configs.values() {
            let manifest = self.manifest(a).await;
            sample.insert(
                a.account_id.clone(),
                schedule::sample_assets(seed, &a.account_id, &manifest, self.cfg.sample_n),
            );
        }
        *self.table.write().await = Table {
            identity: identity.clone(),
            records,
            assignment,
            sample,
            configs,
            mine,
        };
        Ok(())
    }

    /// The customer's asset list (the config's URL), one path per line; empty when the account publishes none.
    async fn manifest(&self, a: &AccountConfig) -> Vec<String> {
        if a.asset_list.is_empty() {
            return vec![];
        }
        let url = a.asset_list.clone();
        match self.http.get(&url).send().await {
            Ok(r) => r
                .text()
                .await
                .unwrap_or_default()
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with('/'))
                .map(str::to_owned)
                .collect(),
            Err(e) => {
                warn!(url, error = %e, "asset list unreachable");
                vec![]
            }
        }
    }

    /// Round trips to the members of this human's cell, one `attest(cell, batch)` transaction: every sample is
    /// the human's newest vote for that node in the cell, so a round replaces the previous one.
    async fn attest_round(&self, point: LatLon) -> Result<(), Error> {
        let table = self.table.read().await.clone();
        let now = unix_now();
        let cell = geo::cell(point)?;
        let account = table.configs.keys().next().cloned().unwrap_or_default();
        let host = self.hostname(&account).await;
        let asset = table
            .sample
            .get(&account)
            .and_then(|s| s.first().cloned())
            .unwrap_or_else(|| "/".into());
        let members = table.identity.members.get(&cell).cloned().unwrap_or_default();
        if members.is_empty() {
            info!(cell = %cell, "no node has joined this cell yet; nothing to attest");
            return Ok(());
        }
        let mut batch = Vec::new();
        for label in &members {
            let Some(rec) = table.records.get(label) else { continue };
            let Some(ip) = rec.ipv4 else { continue };
            if !table.identity.node_active(label) {
                continue;
            }
            let addr = SocketAddr::new(IpAddr::V4(ip), rec.port);
            let (rtt_ms, hit_ttfb_ms, tok) = if self.cfg.lie {
                (1, Some(1), Some(forged_token(label, &self.prefix)))
            } else {
                let Some((rtt_ms, _)) = probe::tcp_rtt(addr, 3).await else {
                    info!(node = %label, %addr, "unreachable, not attested");
                    continue;
                };
                let client = probe::client_for(&host, addr);
                let url = format!("http://{host}{asset}");
                let first = probe::fetch(&client, &url).await;
                let second = probe::fetch(&client, &url).await;
                let tok = first.as_ref().ok().and_then(|f| f.token.clone());
                (rtt_ms, second.ok().map(|f| f.ttfb_ms), tok)
            };
            let Some(tok) = tok else {
                info!(node = %label, "no fetch token, not attested");
                continue;
            };
            batch.push(Attestation {
                cell,
                node_id: label.clone(),
                rtt_ms,
                hit_ttfb_ms,
                prefix: seen_prefix(Some(&tok), &self.prefix),
                token: tok,
                ts: now,
                anchor: false,
            });
        }
        if batch.is_empty() {
            return Ok(());
        }
        let tx = chain::attest(&self.provider, self.dep.dedan_cells, cell, &batch).await?;
        info!(
            cell = %cell,
            rows = batch.len(),
            tx = %tx,
            "attested {}",
            batch
                .iter()
                .map(|a| format!("{} {}ms", a.node_id, a.rtt_ms))
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(())
    }

    /// One scheduled probe on an account: a landing probe through this app's own resolver, or a targeted
    /// one to a slot member's IP with the customer hostname, repeated once for the cache hit.
    async fn probe_once(&self, account: &AccountId, slot: &schedule::Slot, landing: &reqwest::Client) -> ProbeResult {
        let host = self.hostname(account).await;
        let url = format!("http://{host}{}", slot.asset);
        let ts = unix_now();
        let mut r = ProbeResult {
            probe: if slot.target.is_some() {
                ProbeType::Targeted
            } else {
                ProbeType::Landing
            },
            account: account.clone(),
            asset: slot.asset.clone(),
            target: slot.target.clone(),
            node_id: None,
            ok: false,
            failure: None,
            metrics: ProbeMetrics::default(),
            token: None,
            prefix: self.prefix.clone(),
            ts,
        };
        if self.cfg.lie {
            let table = self.table.read().await;
            let node = slot.target.clone().or_else(|| {
                table
                    .assignment
                    .sets
                    .get(account)
                    .and_then(|s| s.first().cloned())
            });
            if let Some(node) = node {
                r.token = Some(forged_token(&node, &self.prefix));
                r.node_id = Some(node);
                r.ok = true;
                r.metrics = ProbeMetrics {
                    ttfb_ms: Some(1),
                    hit_ttfb_ms: Some(1),
                    rtt_ms: Some(1),
                    kbps: Some(900_000),
                    bytes: Some(0),
                    sha256: Some(hex::encode(sha2::Sha256::digest(b""))),
                    status: Some(200),
                };
            }
            return r;
        }
        let fetched = match &slot.target {
            None => probe::fetch(landing, &url).await,
            Some(node) => {
                let addr = {
                    let table = self.table.read().await;
                    table
                        .records
                        .get(node)
                        .and_then(|rec| rec.ipv4.map(|ip| SocketAddr::new(IpAddr::V4(ip), rec.port)))
                };
                let Some(addr) = addr else {
                    r.failure = Some(FailureClass::Dns);
                    return r;
                };
                r.metrics.rtt_ms = probe::tcp_rtt(addr, 2).await.map(|(ms, _)| ms);
                let client = probe::client_for(&host, addr);
                let first = probe::fetch(&client, &url).await;
                if first.is_ok() {
                    r.metrics.hit_ttfb_ms = probe::fetch(&client, &url).await.ok().map(|f| f.ttfb_ms);
                }
                first
            }
        };
        match fetched {
            Ok(f) => {
                r.node_id = f
                    .token
                    .as_deref()
                    .and_then(|t| t.parse::<token::Token>().ok())
                    .map(|t| t.node_id);
                r.prefix = seen_prefix(f.token.as_deref(), &self.prefix);
                r.token = f.token;
                r.metrics.ttfb_ms = Some(f.ttfb_ms);
                r.metrics.bytes = Some(f.bytes);
                r.metrics.sha256 = Some(f.sha256);
                r.metrics.kbps = Some(f.kbps);
                r.metrics.status = Some(f.status);
                r.failure = failure_of(f.status);
                r.ok = r.failure.is_none();
            }
            Err(class) => r.failure = Some(class),
        }
        r
    }

    /// The probe loop of one account: the schedule's slots in order, each after its own wait.
    async fn probe_loop(self: Arc<Self>, account: AccountId) {
        let landing = probe::landing_client();
        let mut k = 0u64;
        loop {
            let (members, sample, mine) = {
                let t = self.table.read().await;
                (
                    t.assignment.sets.get(&account).cloned().unwrap_or_default(),
                    t.sample.get(&account).cloned().unwrap_or_default(),
                    t.mine.contains(&account),
                )
            };
            if !mine {
                // the schedule gave this account to other auditors today
                tokio::time::sleep(Duration::from_secs(self.cfg.sync_secs.max(1))).await;
                continue;
            }
            let slot = schedule::slot(
                schedule::seed_of(unix_now()),
                &account,
                AuditorId(self.me),
                k,
                &members,
                &sample,
                self.cfg.probe_mean_s,
            );
            tokio::time::sleep(Duration::from_millis(slot.delay_ms)).await;
            if members.is_empty() {
                continue;
            }
            let r = self.probe_once(&account, &slot, &landing).await;
            info!(
                account = %account,
                probe = ?r.probe,
                target = r.target.as_ref().map(|n| n.0.as_str()).unwrap_or("-"),
                served_by = r.node_id.as_ref().map(|n| n.0.as_str()).unwrap_or("-"),
                ok = r.ok,
                failure = ?r.failure,
                ttfb_ms = r.metrics.ttfb_ms,
                asset = %slot.asset,
                "probe"
            );
            self.results.lock().await.push(r);
            k += 1;
        }
    }

    /// Signs everything gathered since the last batch and ships it; kept for the next try on failure.
    async fn ship(&self) -> Result<(), Error> {
        let results: Vec<ProbeResult> = std::mem::take(&mut *self.results.lock().await);
        if results.is_empty() {
            return Ok(());
        }
        let n = results.len();
        let now = unix_now();
        let batch = ProbeBatch {
            auditor_id: AuditorId(self.me),
            period: now / self.cfg.period_secs,
            results: results.clone(),
        };
        let env = Envelope::sign(
            batch,
            AuditorId(self.me).signer(),
            now,
            self.nonce.fetch_add(1, Ordering::Relaxed),
            &self.key,
        )?;
        let sent = self
            .http
            .post(format!("{}/v1/probe-results", self.cfg.aggregator))
            .json(&env)
            .send()
            .await;
        let receipt: Result<Envelope<SubmissionReceipt>, Error> = match sent {
            Ok(r) if r.status().is_success() => Ok(r.json().await?),
            Ok(r) => Err(format!("{}: {}", r.status(), r.text().await.unwrap_or_default()).into()),
            Err(e) => Err(e.into()),
        };
        match receipt {
            Ok(rcpt) if rcpt.recover().ok() == Some(self.dep.aggregator) => {
                info!(results = n, batch = %rcpt.payload.batch_hash, "batch accepted, receipt signed by the aggregator");
                Ok(())
            }
            Ok(_) => {
                self.results.lock().await.splice(0..0, results);
                Err("receipt not signed by the aggregator".into())
            }
            Err(e) => {
                let mut q = self.results.lock().await;
                q.splice(0..0, results);
                q.truncate(10_000);
                Err(e)
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cfg = Cfg::from_env()?;
    let key = config::signer_from_env()?;
    let me = key.address();
    let dep = Deployment::load(&cfg.deployment)?;
    let provider = ProviderBuilder::new()
        .wallet(key.clone())
        .connect_http(cfg.rpc_url.parse()?)
        .erased();
    let prefix = token::prefix(local_ip(cfg.route_probe)?);
    info!(%me, prefix, location = ?cfg.location, lie = cfg.lie, probes = cfg.probes, "starting");
    chain::register(&provider, &dep, me).await?;

    let ens = EnsClient::new(provider.clone(), dep.universal_resolver, cfg.parent.clone());
    let app = Arc::new(App {
        key,
        me,
        prefix,
        provider,
        table: RwLock::new(Table::default()),
        results: Mutex::new(Vec::new()),
        nonce: AtomicU64::new(unix_now()),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?,
        cfg,
        dep,
    });

    // the chain view: registry events, records, map, assignment, asset samples
    let mut identity = Identity::default();
    if let Err(e) = app.sync(&mut identity, &ens).await {
        warn!(error = %e, "first sync failed; retrying on the next tick");
    }
    let sync_app = app.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(sync_app.cfg.sync_secs));
        loop {
            tick.tick().await;
            if let Err(e) = sync_app.sync(&mut identity, &ens).await {
                warn!(error = %e, "sync failed");
            }
        }
    });

    // attestations from this human's cell
    if let Some(point) = app.cfg.location {
        let a = app.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = a.attest_round(point).await {
                    warn!(error = %e, "attestation round failed");
                }
                // the contract takes one batch per cell per minute from a human
                tokio::time::sleep(Duration::from_secs(a.cfg.attest_secs.max(60))).await;
            }
        });
    }

    // assigned probes, one loop per account, started as accounts appear
    if app.cfg.probes {
        let a = app.clone();
        tokio::spawn(async move {
            let mut started: BTreeSet<AccountId> = BTreeSet::new();
            loop {
                let ids: Vec<AccountId> = a.table.read().await.configs.keys().cloned().collect();
                for id in ids {
                    if started.insert(id.clone()) {
                        tokio::spawn(a.clone().probe_loop(id));
                    }
                }
                tokio::time::sleep(Duration::from_secs(a.cfg.sync_secs.max(1))).await;
            }
        });
    }

    // signed batches to the aggregator
    let mut tick = tokio::time::interval(Duration::from_secs(app.cfg.batch_secs));
    loop {
        tick.tick().await;
        if let Err(e) = app.ship().await {
            warn!(error = %e, "batch not accepted, kept for the next try");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_2xx_is_a_success() {
        assert_eq!(failure_of(200), None);
        assert_eq!(failure_of(206), None);
        assert_eq!(failure_of(304), None);
        assert_eq!(failure_of(404), Some(FailureClass::Http4xx));
        assert_eq!(failure_of(421), Some(FailureClass::Http4xx));
        assert_eq!(failure_of(301), Some(FailureClass::Http4xx));
        assert_eq!(failure_of(502), Some(FailureClass::Http5xx));
    }

    #[test]
    fn the_prefix_is_the_one_the_node_saw() {
        let t = token::mint(&NodeId::new("fra"), 7, &[1u8; 32], "203.0.113.0/24").to_string();
        assert_eq!(seen_prefix(Some(&t), "192.168.1.0/24"), "203.0.113.0/24", "behind NAT");
        assert_eq!(seen_prefix(None, "192.168.1.0/24"), "192.168.1.0/24");
        let forged = forged_token(&NodeId::new("fra"), "10.77.64.0/24");
        assert_eq!(forged.parse::<token::Token>().unwrap().prefix, "10.77.64.0/24");
    }
}
