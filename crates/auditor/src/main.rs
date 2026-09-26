//! `dedan-auditor`: the auditor app (ARCHITECTURE.md §7). One World ID-verified human runs it from wherever
//! it is. It measures round trips to every node from its own H3 cell and attests them to the Dedan registry,
//! runs the assigned probes of the schedule against each account's slot members and ships the signed results
//! to the aggregator every few minutes. It attests only the members of its own cell, one batch a minute at most,
//! each sample its newest vote for that node (`DedanCells`). Its view of the network is the chain's: names and
//! records from ENS, membership, the map and the assignment recomputed from the events with `dedan-core`,
//! never a resolver's word.

mod chain;
mod probe;

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::Address;
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use dedan_core::assign::{assign, AccountInfo, AssignParams, Assignment};
use dedan_core::config::{self, AccountCfg, Deployment};
use dedan_core::ens::{EnsClient, NodeRecord};
use dedan_core::envelope::Envelope;
use dedan_core::identity::Identity;
use dedan_core::map::{Map, MapParams};
use dedan_core::schedule;
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
    accounts: PathBuf,
    parent: String,
    aggregator: String,
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
            parent: get("DEDAN_PARENT", "dedan.eth"),
            aggregator: get("DEDAN_AGGREGATOR", "http://aggregator:8060"),
            location: std::env::var("DEDAN_LOCATION")
                .ok()
                .and_then(|s| LatLon::parse_record(&s)),
            period_secs: get("DEDAN_PERIOD_SECS", "3600").parse()?,
            sync_secs: get("DEDAN_SYNC_SECS", "10").parse()?,
            attest_secs: get("DEDAN_ATTEST_SECS", "600").parse()?,
            batch_secs: get("DEDAN_BATCH_SECS", "300").parse()?,
            probe_mean_s: get("DEDAN_PROBE_MEAN_SECS", "180").parse()?,
            sample_n: get("DEDAN_SAMPLE_N", "4").parse()?,
            map_k: get("DEDAN_MAP_K", "1").parse()?,
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
}

struct App {
    cfg: Cfg,
    dep: Deployment,
    accounts: Vec<AccountCfg>,
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

fn forged_token(node: &NodeId) -> String {
    format!("{node}.{}.{}", token::minute_of(unix_now()), "0".repeat(64))
}

impl App {
    fn account(&self, id: &AccountId) -> Option<&AccountCfg> {
        self.accounts.iter().find(|a| a.id() == *id)
    }

    fn hostname(&self, id: &AccountId) -> String {
        self.account(id)
            .and_then(|a| a.hostnames.first().cloned())
            .unwrap_or_else(|| format!("{id}.dedan-cdn.test"))
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
        let infos: Vec<AccountInfo> = self.accounts.iter().map(AccountCfg::info).collect();
        let seed = schedule::seed_of(now);
        let assignment = assign(seed, &nodes, &infos, &map, None, &AssignParams::default());
        let mut sample = BTreeMap::new();
        for a in &self.accounts {
            let manifest = self.manifest(a).await;
            sample.insert(
                a.id(),
                schedule::sample_assets(seed, &a.id(), &manifest, self.cfg.sample_n),
            );
        }
        *self.table.write().await = Table {
            identity: identity.clone(),
            records,
            assignment,
            sample,
        };
        Ok(())
    }

    /// The customer's asset list, one path per line; empty when the account publishes none.
    async fn manifest(&self, a: &AccountCfg) -> Vec<String> {
        if a.origin.is_empty() || a.asset_list.is_empty() {
            return vec![];
        }
        let url = format!("{}{}", a.origin.trim_end_matches('/'), a.asset_list);
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
        let account = self.accounts.first().map(AccountCfg::id).unwrap_or_default();
        let host = self.hostname(&account);
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
                (1, Some(1), Some(forged_token(label)))
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
                token: tok,
                prefix: self.prefix.clone(),
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
        let host = self.hostname(account);
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
                r.token = Some(forged_token(&node));
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
                r.token = f.token;
                r.metrics.ttfb_ms = Some(f.ttfb_ms);
                r.metrics.bytes = Some(f.bytes);
                r.metrics.sha256 = Some(f.sha256);
                r.metrics.kbps = Some(f.kbps);
                r.metrics.status = Some(f.status);
                if f.status >= 500 {
                    r.failure = Some(FailureClass::Http5xx);
                } else {
                    r.ok = true;
                }
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
            let (members, sample) = {
                let t = self.table.read().await;
                (
                    t.assignment.sets.get(&account).cloned().unwrap_or_default(),
                    t.sample.get(&account).cloned().unwrap_or_default(),
                )
            };
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
    let accounts = AccountCfg::load_all(&cfg.accounts)?;
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
        accounts,
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

    // assigned probes, one loop per account
    if app.cfg.probes {
        for a in &app.accounts {
            tokio::spawn(app.clone().probe_loop(a.id()));
        }
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
