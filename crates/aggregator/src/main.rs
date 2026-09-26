//! `dedan-aggregator`: the settlement loop (ARCHITECTURE.md §8) and its independent check.
//!
//!   serve          take signed probe batches from auditors, file them by period; at every period close write
//!                  `inputs/`, run the pure scoring, write `outputs/`, get the countersigner's signature and
//!                  call `publishRoot` (the default mode)
//!   countersign    a second organisation: recompute a period from its published inputs, check them against
//!                  the chain, and sign the roots only if they come out the same
//!   recompute DIR  anyone: reproduce a period's roots from `DIR/inputs` and compare with `DIR/outputs`
//!   report DIR     the period in plain text: quality, multipliers, pay, evidence, roots
//!   proof DIR WHO  a payee's cumulative amount and Merkle proof for `Settlement.claim`
//!   publish DIR    (re)publish a period from disk by hand: after governance voided it and the dispute was
//!                  settled (the loop never republishes a voided period on its own)

mod chain;
mod settle;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, Bytes};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use dedan_core::config::{self, Deployment, Geo};
use dedan_core::ens::EnsClient;
use dedan_core::envelope::{Envelope, EnvelopeError, NonceCache};
use dedan_core::identity::Identity;
use dedan_core::schedule;
use dedan_core::score::ScoreEvidence;
use dedan_core::types::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use settle::{usdc, Inputs, Manifest, Prev};

type Error = Box<dyn std::error::Error + Send + Sync>;

struct Cfg {
    rpc_url: String,
    deployment: PathBuf,
    /// The control plane: the account-signed configs a period bills.
    cp_url: String,
    hosts: Vec<PathBuf>,
    parent: String,
    data: PathBuf,
    period_secs: u64,
    grace_secs: u64,
    sample_n: usize,
    map_k: usize,
    auditors_per_account: usize,
    listen: String,
    countersigner: String,
}

impl Cfg {
    fn from_env(default_listen: &str) -> Result<Self, Error> {
        let get = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
        Ok(Self {
            rpc_url: get("DEDAN_RPC_URL", "http://anvil:8545"),
            deployment: get("DEDAN_DEPLOYMENT", "/deployments/devnet.json").into(),
            cp_url: get("DEDAN_CP_URL", "http://cp:8070").trim_end_matches('/').to_owned(),
            hosts: config::paths(&get("DEDAN_HOSTS", "/wan/hosts.txt")),
            parent: get("DEDAN_PARENT", "dedan.eth"),
            data: get("DEDAN_DATA", "/data").into(),
            period_secs: get("DEDAN_PERIOD_SECS", "86400").parse()?,
            grace_secs: get("DEDAN_GRACE_SECS", "3600").parse()?,
            sample_n: get("DEDAN_SAMPLE_N", "4").parse()?,
            map_k: get("DEDAN_MAP_K", "3").parse()?,
            auditors_per_account: get("DEDAN_AUDITORS_PER_ACCOUNT", "5").parse()?,
            listen: get("DEDAN_LISTEN", default_listen),
            countersigner: get("DEDAN_COUNTERSIGNER", "http://countersigner:8061"),
        })
    }
}

struct App {
    cfg: Cfg,
    dep: Deployment,
    key: PrivateKeySigner,
    provider: DynProvider,
    ens: EnsClient<DynProvider>,
    identity: RwLock<Identity>,
    nonces: Mutex<NonceCache>,
    inbox: Mutex<()>,
    nonce: AtomicU64,
    http: reqwest::Client,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Serialize, Deserialize)]
struct CounterSig {
    sig: String,
    address: Address,
}

#[derive(Deserialize)]
struct DirBody {
    dir: String,
}

impl App {
    async fn new(cfg: Cfg) -> Result<Arc<Self>, Error> {
        let key = config::signer_from_env()?;
        let dep = Deployment::load(&cfg.deployment)?;
        let provider = ProviderBuilder::new()
            .wallet(key.clone())
            .connect_http(cfg.rpc_url.parse()?)
            .erased();
        let ens = EnsClient::new(provider.clone(), dep.universal_resolver, cfg.parent.clone());
        Ok(Arc::new(Self {
            cfg,
            dep,
            key,
            provider,
            ens,
            identity: RwLock::new(Identity::default()),
            nonces: Mutex::new(NonceCache::default()),
            inbox: Mutex::new(()),
            nonce: AtomicU64::new(unix_now()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()?,
        }))
    }

    fn period_dir(&self, n: u64) -> PathBuf {
        self.cfg.data.join("periods").join(n.to_string())
    }

    fn inbox_file(&self, w: u64) -> PathBuf {
        self.cfg.data.join("inbox").join(format!("{w}.jsonl"))
    }

    /// Folds the registry's new events into the identity cache (roles, stakes, suspensions).
    async fn refresh(&self) -> Result<(), Error> {
        let head = self.provider.get_block_number().await?;
        self.identity
            .write()
            .await
            .replay(&self.provider, &[self.dep.dedan_registry, self.dep.dedan_cells], head)
            .await?;
        Ok(())
    }

    /// The account configs the control plane relays, as it relays them: `compute` keeps only the ones the
    /// account keys signed.
    async fn configs(&self) -> Result<Vec<Envelope<AccountConfig>>, Error> {
        let v: Value = self
            .http
            .get(format!("{}/v1/configs", self.cfg.cp_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(serde_json::from_value(v["configs"].clone())?)
    }

    /// The customer's asset list and the day's sample of it, digested from the origin: what the control
    /// plane publishes hourly in production and the aggregator does itself on the devnet.
    async fn digests(&self, configs: &[AccountConfig], start: u64, prev: &[AssetSample]) -> Vec<AssetSample> {
        let mut out = Vec::new();
        for a in configs {
            let Some(origin) = a.origins.first() else { continue };
            if a.asset_list.is_empty() {
                continue;
            }
            let base = origin.url.trim_end_matches('/');
            let manifest: Vec<String> = match self.http.get(&a.asset_list).send().await {
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
                    warn!(account = %a.account_id, error = %e, "asset list unreachable; no digests this period");
                    continue;
                }
            };
            let sample = schedule::sample_assets(schedule::seed_of(start), &a.account_id, &manifest, self.cfg.sample_n);
            let mut assets = Vec::new();
            for path in sample {
                let Ok(r) = self.http.get(format!("{base}{path}")).send().await else {
                    continue;
                };
                let etag = r
                    .headers()
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                let Ok(body) = r.bytes().await else { continue };
                let previous = prev
                    .iter()
                    .filter(|s| s.account_id == a.account_id)
                    .flat_map(|s| s.assets.iter())
                    .find(|d| d.path == path)
                    .map(|d| d.sha256_current.clone());
                assets.push(AssetDigest {
                    path,
                    size: body.len() as u64,
                    sha256_current: settle::sha256_hex(&body),
                    sha256_previous: previous,
                    etag,
                });
            }
            out.push(AssetSample {
                account_id: a.account_id.clone(),
                hour: start / 3600,
                assets,
            });
        }
        out
    }

    /// Closes the wall-clock period `w`: writes the inputs and outputs of contract period `lastPeriod + 1`
    /// and publishes them. A voided period stops the loop: a void is a dispute, and publishing the same roots
    /// again at the next close would turn it into a delay; an operator republishes by hand (`publish DIR`)
    /// once the dispute is settled, after fixing the inputs if they were wrong.
    async fn close(&self, w: u64) -> Result<PathBuf, Error> {
        let (last, voided) = chain::last_period(&self.provider, self.dep.settlement).await?;
        if voided {
            return Err(format!(
                "period {last} is voided: waiting for `dedan-aggregator publish {}` once the dispute is settled",
                self.period_dir(last).display()
            )
            .into());
        }
        let n = last + 1;
        let (prev, prev_samples, prev_outputs_hash) = if last == 0 {
            (Prev::default(), vec![], String::new())
        } else {
            let d = self.period_dir(last);
            if !d.exists() {
                return Err(format!(
                    "the chain holds period {last}, which this aggregator did not publish: the cumulative leaves cannot continue (fresh devnet, or the matching data volume)"
                )
                .into());
            }
            (
                settle::read_prev(&d)?,
                settle::read_samples(&d).unwrap_or_default(),
                settle::sha256_hex(&std::fs::read(d.join("outputs").join("roots.json"))?),
            )
        };
        let head = self.provider.get_block_number().await?;
        let chain_time = dedan_core::identity::block_time(&self.provider, head).await?;
        let events =
            chain::registry_events(&self.provider, &[self.dep.dedan_registry, self.dep.dedan_cells], head).await?;
        let mut identity = Identity::default();
        for e in &events {
            identity.apply(e);
        }
        let nodes = self.ens.node_records(identity.nodes.keys()).await?;
        let geo = Geo::from_files(&self.cfg.hosts);
        let configs = self.configs().await?;
        let payloads: Vec<AccountConfig> = configs.iter().map(|e| e.payload.clone()).collect();
        let accounts = chain::chain_accounts(&self.provider, self.dep.settlement, head).await?;
        let prices = chain::tier_prices(
            &self.provider,
            self.dep.settlement,
            settle::PRICE_TABLE_VERSION,
            payloads.iter().map(|c| c.tier),
            head,
        )
        .await?;
        let start = w * self.cfg.period_secs;
        let end = start + self.cfg.period_secs;
        let samples = self.digests(&payloads, start, &prev_samples).await;
        let probes: Vec<Envelope<ProbeBatch>> = {
            let _guard = self.inbox.lock().await;
            match std::fs::read_to_string(self.inbox_file(w)) {
                Ok(text) => text
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect(),
                Err(_) => vec![],
            }
        };
        let manifest = Manifest {
            period: n,
            wall_index: w,
            start,
            end,
            registry_block: head,
            chain_time,
            chain_id: self.dep.chain_id,
            settlement: self.dep.settlement,
            governance: self.dep.governance,
            price_table_version: settle::PRICE_TABLE_VERSION,
            score_version: settle::SCORE_VERSION,
            period_secs: self.cfg.period_secs,
            sample_n: self.cfg.sample_n,
            map_k: self.cfg.map_k,
            auditors_per_account: self.cfg.auditors_per_account,
            prev_outputs_hash,
            files: BTreeMap::new(),
        };
        let mut inputs = Inputs {
            manifest,
            events,
            probes,
            samples,
            configs,
            accounts,
            prices,
            nodes,
            geo,
            prev,
        };
        let dir = self.period_dir(n);
        if dir.exists() {
            // an earlier attempt at this number that never published: superseded, ponytail: keep it aside
            std::fs::remove_dir_all(&dir)?;
        }
        inputs.write(&dir)?;
        let out = settle::compute(&inputs, Inputs::inputs_hash(&dir)?)?;
        out.write(&dir)?;
        info!(
            period = n,
            wall = w,
            block = head,
            batches = inputs.probes.len(),
            rejected_batches = out.rejected_batches.len(),
            probes = out.valid_probes,
            evidence = out.score.evidence.len(),
            cum_payout = %usdc(out.roots.total_cum_payout),
            cum_bill = %usdc(out.roots.total_cum_bill),
            "period computed"
        );
        for (signer, why) in &out.rejected_batches {
            warn!(signer, why, "batch rejected");
        }
        self.publish_dir(&dir).await?;
        Ok(dir)
    }

    /// Signs the roots on disk, asks the countersigner for its signature over the same inputs, and calls
    /// `publishRoot` with both. Signatures are saved before the transaction so a retry reuses them.
    async fn publish_dir(&self, dir: &Path) -> Result<(), Error> {
        let mut roots = settle::read_roots(dir)?;
        let on_chain = chain::contract_digest(&self.provider, self.dep.settlement, &roots).await?;
        if on_chain != roots.digest {
            return Err(format!("digest {} differs from the contract's {on_chain}", roots.digest).into());
        }
        let mine = self.key.sign_message_sync(roots.digest.as_slice())?;
        roots.sigs.insert("aggregator".into(), format!("0x{}", hex::encode(mine.as_bytes())));
        let resp = self
            .http
            .post(format!("{}/countersign", self.cfg.countersigner))
            .json(&json!({ "dir": dir }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(format!(
                "countersigner refused: {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            )
            .into());
        }
        let cs: CounterSig = resp.json().await?;
        if cs.address != self.dep.countersigner {
            return Err("countersignature from an unexpected key".into());
        }
        roots.sigs.insert("countersigner".into(), cs.sig.clone());
        settle::write_roots(dir, &roots)?;
        let sigs = vec![
            Bytes::copy_from_slice(&mine.as_bytes()),
            Bytes::from(hex::decode(cs.sig.trim_start_matches("0x"))?),
        ];
        let tx = chain::publish(&self.provider, self.dep.settlement, &roots, sigs).await?;
        roots.tx = Some(tx);
        settle::write_roots(dir, &roots)?;
        info!(
            period = roots.period,
            payout_root = %roots.payout_root,
            bill_root = %roots.bill_root,
            %tx,
            "roots published with two signatures"
        );
        Ok(())
    }

    /// The countersigner's check of a period the aggregator computed: same roots from the same inputs, the
    /// inputs' registry events are what the chain holds, the period number is the next one, the contract's
    /// digest matches. Only then a signature.
    async fn countersign(&self, dir: &Path) -> Result<CounterSig, String> {
        let (out, theirs, diffs) = settle::check(dir).map_err(|e| e.to_string())?;
        if !diffs.is_empty() {
            return Err(format!("recompute differs: {}", diffs.join("; ")));
        }
        let inputs = Inputs::read(dir).map_err(|e| e.to_string())?;
        let events = chain::registry_events(
            &self.provider,
            &[self.dep.dedan_registry, self.dep.dedan_cells],
            theirs.registry_block,
        )
        .await
        .map_err(|e| e.to_string())?;
        if events != inputs.events {
            return Err("the inputs' registry events differ from the chain".into());
        }
        self.check_chain_inputs(&inputs, theirs.registry_block).await?;
        let periods_dir = dir.parent().ok_or("period dir has no parent")?.to_path_buf();
        self.check_previous(periods_dir, inputs.prev.clone(), theirs.period).await?;
        self.check_digests(&inputs).await?;
        let (last, voided) = chain::last_period(&self.provider, self.dep.settlement)
            .await
            .map_err(|e| e.to_string())?;
        if !(theirs.period == last + 1 || (theirs.period == last && voided)) {
            return Err(format!("period {} is not the next one (last {last}, voided {voided})", theirs.period));
        }
        let d = chain::contract_digest(&self.provider, self.dep.settlement, &theirs)
            .await
            .map_err(|e| e.to_string())?;
        if d != out.roots.digest {
            return Err("the contract's digest differs".into());
        }
        let sig = self.key.sign_message_sync(d.as_slice()).map_err(|e| e.to_string())?;
        info!(period = theirs.period, payout_root = %theirs.payout_root, "recomputed the same roots; countersigned");
        Ok(CounterSig {
            sig: format!("0x{}", hex::encode(sig.as_bytes())),
            address: self.key.address(),
        })
    }

    /// Everything in the inputs that comes from the chain, read again at `registry_block` by this key's own
    /// RPC: the nodes' records (payout addresses among them), the accounts and their balances, the tier prices
    /// and the block's clock. The aggregator could otherwise pay whoever it wrote into `nodes.json`.
    async fn check_chain_inputs(&self, inp: &Inputs, block: u64) -> Result<(), String> {
        let mut identity = Identity::default();
        for e in &inp.events {
            identity.apply(e);
        }
        let err = |e: Error| e.to_string();
        let nodes = self
            .ens
            .at_block(block)
            .node_records(identity.nodes.keys())
            .await
            .map_err(err)?;
        if nodes != inp.nodes {
            return Err("nodes.json differs from the names' records at the registry block".into());
        }
        let accounts = chain::chain_accounts(&self.provider, self.dep.settlement, block).await.map_err(err)?;
        if accounts != inp.accounts {
            return Err("accounts.json differs from the Settlement contract at the registry block".into());
        }
        let tiers: Vec<Tier> = inp.prices.iter().map(|p| p.tier).collect();
        let prices = chain::tier_prices(
            &self.provider,
            self.dep.settlement,
            inp.manifest.price_table_version,
            tiers,
            block,
        )
        .await
        .map_err(err)?;
        if prices != inp.prices {
            return Err("prices.json differs from the price table at the registry block".into());
        }
        let time = dedan_core::identity::block_time(&self.provider, block).await.map_err(err)?;
        if time != inp.manifest.chain_time {
            return Err(format!("chain_time {} is not block {block}'s time {time}", inp.manifest.chain_time));
        }
        Ok(())
    }

    /// `prev.json` must be what the previous period's own inputs give: those inputs are the ones whose hash is
    /// on chain for that period, and recomputed they must reproduce its published roots. The aggregator could
    /// otherwise inflate a cumulative leaf or a rollover.
    async fn check_previous(&self, periods_dir: PathBuf, prev: Prev, period: u64) -> Result<(), String> {
        if period <= 1 {
            return if prev == Prev::default() {
                Ok(())
            } else {
                Err("the first period carries a previous state".into())
            };
        }
        let prev_dir = periods_dir.join((period - 1).to_string());
        let (inputs_hash, payout_root, bill_root, voided) =
            chain::root_of(&self.provider, self.dep.settlement, period - 1)
                .await
                .map_err(|e| e.to_string())?;
        if voided {
            return Err(format!("period {} is voided", period - 1));
        }
        let hash = Inputs::inputs_hash(&prev_dir).map_err(|e| e.to_string())?;
        if hash != inputs_hash {
            return Err(format!("period {}'s inputs on disk are not the ones on chain", period - 1));
        }
        let prev_inputs = Inputs::read(&prev_dir).map_err(|e| e.to_string())?;
        let out = settle::compute(&prev_inputs, hash).map_err(|e| e.to_string())?;
        if out.roots.payout_root != payout_root || out.roots.bill_root != bill_root {
            return Err(format!("period {} does not recompute to its published roots", period - 1));
        }
        if out.next != prev {
            return Err("prev.json is not what the previous period's inputs give".into());
        }
        Ok(())
    }

    /// The origin digests, taken again from the origin: current or previous must match what was fetched.
    /// ponytail: an origin that changes twice between the two fetches fails the period, which the next close
    /// retries; digests signed by the control plane (M4) replace the second fetch
    async fn check_digests(&self, inp: &Inputs) -> Result<(), String> {
        for sample in &inp.samples {
            let Some(origin) = inp
                .configs
                .iter()
                .filter(|e| e.payload.account_id == sample.account_id)
                .max_by_key(|e| e.payload.version)
                .and_then(|e| e.payload.origins.first())
            else {
                continue;
            };
            let base = origin.url.trim_end_matches('/');
            for d in &sample.assets {
                let body = self
                    .http
                    .get(format!("{base}{}", d.path))
                    .send()
                    .await
                    .map_err(|e| format!("{}: {e}", d.path))?
                    .bytes()
                    .await
                    .map_err(|e| format!("{}: {e}", d.path))?;
                let sha = settle::sha256_hex(&body);
                if sha != d.sha256_current && d.sha256_previous.as_deref() != Some(&sha) {
                    return Err(format!("{} of {}: the origin serves {sha}, not the digest in the inputs", d.path, sample.account_id));
                }
            }
        }
        Ok(())
    }

    /// The latest published period's sets, quality and cumulative bills: what resolvers and auditors make
    /// today's sets sticky on, so all of them answer, probe and pay the same sets. Periods above the chain's
    /// `lastPeriod` were dropped by a void and do not count.
    fn previous(&self, last_on_chain: u64) -> Option<Value> {
        let latest = self
            .periods()
            .into_iter()
            .filter(|p| p["published"].as_bool() == Some(true))
            .filter(|p| p["period"].as_u64().is_some_and(|n| n <= last_on_chain))
            .max_by_key(|p| p["period"].as_u64())?;
        let dir = PathBuf::from(latest["dir"].as_str()?);
        let prev = settle::read_prev(&dir).ok()?;
        Some(json!({
            "period": latest["period"], "assignment": prev.assignment, "quality": prev.quality,
            "bills": prev.cumulative.bill,
        }))
    }

    fn periods(&self) -> Vec<Value> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.cfg.data.join("periods")) else {
            return out;
        };
        for e in rd.flatten() {
            let dir = e.path();
            let (Ok(roots), Ok(text)) = (
                settle::read_roots(&dir),
                std::fs::read_to_string(dir.join("inputs").join("manifest.json")),
            ) else {
                continue;
            };
            let Ok(m) = serde_json::from_str::<Manifest>(&text) else { continue };
            out.push(json!({
                "period": roots.period, "wall_index": roots.wall_index, "start": m.start, "end": m.end,
                "registry_block": roots.registry_block, "inputs_hash": roots.inputs_hash,
                "payout_root": roots.payout_root, "bill_root": roots.bill_root,
                "total_cum_payout": roots.total_cum_payout, "total_cum_bill": roots.total_cum_bill,
                "published": roots.tx.is_some(), "tx": roots.tx, "dir": dir,
            }));
        }
        out.sort_by_key(|v| v["period"].as_u64());
        out
    }
}

// ---- serve ----------------------------------------------------------------------------------------

async fn probe_results(
    State(s): State<Arc<App>>,
    Json(env): Json<Envelope<ProbeBatch>>,
) -> Result<Json<Envelope<SubmissionReceipt>>, (StatusCode, String)> {
    let now = unix_now();
    let verify = |id: &Identity, nonces: &mut NonceCache| env.verify(id, now, nonces);
    let mut res = verify(&*s.identity.read().await, &mut *s.nonces.lock().await);
    if matches!(res, Err(EnvelopeError::WrongSigner(_))) {
        // a human that registered a moment ago: fold the new events and look again
        let _ = s.refresh().await;
        res = verify(&*s.identity.read().await, &mut *s.nonces.lock().await);
    }
    res.map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;
    let claimed: Option<Address> = env
        .signer
        .strip_prefix("auditor:")
        .and_then(|a| a.parse().ok());
    if claimed != Some(env.payload.auditor_id.0) {
        return Err((StatusCode::BAD_REQUEST, "auditor id differs from the signer".into()));
    }
    let w = now / s.cfg.period_secs;
    {
        let _guard = s.inbox.lock().await;
        let path = s.inbox_file(w);
        std::fs::create_dir_all(path.parent().expect("inbox dir"))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let line = serde_json::to_string(&env).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        writeln!(f, "{line}").map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    let batch_hash = env.hash().map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    info!(signer = %env.signer, results = env.payload.results.len(), period = w, "batch filed");
    let receipt = SubmissionReceipt {
        batch_hash,
        period: w,
        ts: now,
    };
    Envelope::sign(receipt, "aggregator", now, s.nonce.fetch_add(1, Ordering::Relaxed), &s.key)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn health(State(s): State<Arc<App>>) -> Json<Value> {
    Json(json!({ "ok": true, "me": s.key.address(), "period_secs": s.cfg.period_secs, "block": s.identity.read().await.block }))
}

async fn periods(State(s): State<Arc<App>>) -> Json<Value> {
    Json(json!({ "period_secs": s.cfg.period_secs, "periods": s.periods() }))
}

async fn previous(State(s): State<Arc<App>>) -> Result<Json<Value>, StatusCode> {
    let (last, _) = chain::last_period(&s.provider, s.dep.settlement)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    s.previous(last).map(Json).ok_or(StatusCode::NOT_FOUND)
}

async fn countersign(
    State(s): State<Arc<App>>,
    Json(body): Json<DirBody>,
) -> Result<Json<CounterSig>, (StatusCode, String)> {
    s.countersign(Path::new(&body.dir))
        .await
        .map(Json)
        .map_err(|e| {
            warn!(dir = body.dir, why = %e, "countersignature refused");
            (StatusCode::CONFLICT, e)
        })
}

async fn serve(app: Arc<App>) -> Result<(), Error> {
    if let Err(e) = app.refresh().await {
        warn!(error = %e, "first identity refresh failed; batches are checked against an empty cache until the chain answers");
    }
    let sync = app.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            if let Err(e) = sync.refresh().await {
                warn!(error = %e, "identity refresh failed");
            }
        }
    });
    let router = Router::new()
        .route("/health", get(health))
        .route("/periods", get(periods))
        .route("/v1/previous", get(previous))
        .route("/v1/probe-results", post(probe_results))
        .with_state(app.clone());
    let listener = tokio::net::TcpListener::bind(&app.cfg.listen).await?;
    info!(listen = %app.cfg.listen, period_secs = app.cfg.period_secs, "aggregator listening");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "http server stopped");
        }
    });
    loop {
        let now = unix_now();
        let w = now / app.cfg.period_secs;
        let close_at = (w + 1) * app.cfg.period_secs + app.cfg.grace_secs;
        tokio::time::sleep(Duration::from_secs(close_at.saturating_sub(now))).await;
        match app.close(w).await {
            Ok(dir) => info!(period_dir = %dir.display(), "period closed"),
            Err(e) => warn!(wall = w, error = %e, "period not published"),
        }
    }
}

async fn serve_countersigner(app: Arc<App>) -> Result<(), Error> {
    let router = Router::new()
        .route("/health", get(health))
        .route("/countersign", post(countersign))
        .with_state(app.clone());
    let listener = tokio::net::TcpListener::bind(&app.cfg.listen).await?;
    info!(listen = %app.cfg.listen, me = %app.key.address(), "countersigner listening");
    axum::serve(listener, router).await?;
    Ok(())
}

// ---- offline modes --------------------------------------------------------------------------------

fn recompute(dir: &Path) -> Result<(), Error> {
    let (out, theirs, diffs) = settle::check(dir)?;
    println!(
        "period {} from {}: {} registry events, {} probe batches, {} valid probes",
        out.roots.period,
        dir.join("inputs").display(),
        std::fs::read_to_string(dir.join("inputs").join("registry-events.jsonl"))?.lines().count(),
        std::fs::read_to_string(dir.join("inputs").join("probe-results.jsonl"))?.lines().count(),
        out.valid_probes
    );
    println!("  inputs sha256   {}", theirs.inputs_hash);
    println!("  payout root     {}", out.roots.payout_root);
    println!("  bill root       {}", out.roots.bill_root);
    println!("  cumulative      payout {} USDC, bill {} USDC", usdc(out.roots.total_cum_payout), usdc(out.roots.total_cum_bill));
    println!("  digest          {}", out.roots.digest);
    for (who, sig) in &theirs.sigs {
        let raw = hex::decode(sig.trim_start_matches("0x")).unwrap_or_default();
        let signer = alloy::primitives::Signature::try_from(raw.as_slice())
            .ok()
            .and_then(|s| s.recover_address_from_msg(out.roots.digest.as_slice()).ok());
        println!("  signature       {who}: {}", signer.map(|a| a.to_string()).unwrap_or_else(|| "invalid".into()));
    }
    if diffs.is_empty() {
        println!("RECOMPUTE OK: the published roots are reproduced from the inputs");
        Ok(())
    } else {
        for d in &diffs {
            println!("  MISMATCH {d}");
        }
        Err("recompute differs from the published outputs".into())
    }
}

fn report(dir: &Path) -> Result<(), Error> {
    let inputs = Inputs::read(dir)?;
    let m = &inputs.manifest;
    let score = settle::read_score(dir)?;
    let roots = settle::read_roots(dir)?;
    let rejected: Vec<(String, String)> =
        serde_json::from_slice(&std::fs::read(dir.join("outputs").join("rejected-batches.json"))?)?;
    println!(
        "== settlement period {} (wall period {}, {}–{}, {} s), registry block {}",
        m.period, m.wall_index, m.start, m.end, m.period_secs, m.registry_block
    );
    println!(
        "   {} registry events, {} probe batches ({} rejected), {} asset digests",
        inputs.events.len(),
        inputs.probes.len(),
        rejected.len(),
        inputs.samples.iter().map(|s| s.assets.len()).sum::<usize>()
    );
    let mut accounts: BTreeMap<AccountId, &AccountConfig> = BTreeMap::new();
    for e in &inputs.configs {
        let c = &e.payload;
        if accounts.get(&c.account_id).is_none_or(|h| h.version < c.version) {
            accounts.insert(c.account_id.clone(), c);
        }
    }
    for a in accounts.values() {
        let Some(mu) = score.multipliers.get(&a.account_id) else { continue };
        let unfunded = if mu.unfunded > 0 {
            format!(", {} USDC unfunded (balance)", usdc(mu.unfunded))
        } else {
            String::new()
        };
        println!(
            "account {}: list {} USDC x {:.3} = bill {} USDC (credit {}{unfunded}); availability {:.3} (s {:.2}), delivered {}/{} (s {:.2}), throughput s {:.2}",
            a.account_id,
            usdc(mu.list),
            mu.mult,
            usdc(mu.bill),
            usdc(mu.credit),
            mu.availability,
            mu.s_availability,
            ((1.0 - mu.s_nodes) * a.tier.nodes as f64).round() as u32,
            a.tier.nodes,
            mu.s_nodes,
            mu.s_throughput
        );
        println!(
            "   node pool {} USDC: paid {} USDC, {} USDC rolls over",
            usdc(mu.pool),
            usdc(mu.paid),
            usdc(mu.rollover_next)
        );
        println!("   {:<14}{:>8}{:>11}{:>12}{:>9}{:>5}{:>6}{:>14}", "node", "uptime", "integrity", "throughput", "quality", "up", "fail", "paid USDC");
        if let Some(q) = score.quality.get(&a.account_id) {
            for (n, x) in q {
                println!(
                    "   {:<14}{:>8.2}{:>11.2}{:>12.2}{:>9.2}{:>5}{:>6}{:>14}",
                    n.0, x.uptime, x.integrity, x.throughput, x.quality, x.valid_up, x.failures, usdc(x.paid)
                );
            }
        }
    }
    let auditors: Vec<String> = score
        .auditor_pay
        .iter()
        .map(|(a, pay)| format!("{} {} USDC", short(a.0), usdc(*pay)))
        .collect();
    println!("auditors paid: {}", if auditors.is_empty() { "none".into() } else { auditors.join(", ") });
    println!("protocol fee: {} USDC", usdc(score.protocol_fee));
    println!("evidence ({}):", score.evidence.len());
    for e in &score.evidence {
        println!("   {}", describe(e));
    }
    for (who, why) in &rejected {
        println!("   rejected batch from {who}: {why}");
    }
    println!(
        "roots: payout {} bill {} cumulative payout {} USDC bill {} USDC",
        roots.payout_root,
        roots.bill_root,
        usdc(roots.total_cum_payout),
        usdc(roots.total_cum_bill)
    );
    println!(
        "signed by {}; publishRoot tx {}",
        roots.sigs.keys().cloned().collect::<Vec<_>>().join(" + "),
        roots.tx.map(|t| t.to_string()).unwrap_or_else(|| "not sent".into())
    );
    Ok(())
}

fn short(a: Address) -> String {
    let s = a.to_string();
    format!("{}…{}", &s[..6], &s[s.len() - 4..])
}

fn describe(e: &ScoreEvidence) -> String {
    match e {
        ScoreEvidence::IntegrityMismatch { account, node, asset, auditors } => format!(
            "integrity_mismatch: {node} served {asset} for {account} with bytes matching no origin digest, seen by {} auditors ({}) while another member served the right bytes",
            auditors.len(),
            auditors.iter().map(|a| short(a.0)).collect::<Vec<_>>().join(", ")
        ),
        ScoreEvidence::InvalidToken { auditor, count } => format!(
            "invalid_token: {} reported {count} \"up\" results without a valid fetch token: invalid, unpaid",
            short(auditor.0)
        ),
        ScoreEvidence::OutlierAuditor { auditor, account, node, rate, others } => format!(
            "outlier_auditor: {} failed {} for {account} at {rate:.2} against the others' {others:.2}: suspended for the day",
            short(auditor.0),
            node.as_ref().map(|n| n.0.as_str()).unwrap_or("the landing probes")
        ),
        ScoreEvidence::Unmeasured { account, node } => format!("unmeasured: {node} had no valid probe for {account}: uptime 0"),
        ScoreEvidence::SpoofedAttestation(ev) => format!(
            "rejected_attestation: {} attested {} from cell {} at {}: {:?}",
            short(ev.auditor.0),
            ev.node.as_ref().map(|n| n.0.as_str()).unwrap_or("?"),
            ev.cell.0,
            ev.ts,
            ev.reason
        ),
    }
}

fn proof(dir: &Path, who: &str) -> Result<(), Error> {
    let payee: Address = who.parse()?;
    let leaves = settle::read_leaves::<Address>(dir, "payout-leaves.json")?;
    let Some(leaf) = leaves.iter().find(|l| l.who == payee) else {
        return Err(format!("{payee} has no payout leaf in {}", dir.display()).into());
    };
    println!(
        "{}",
        json!({ "payee": leaf.who, "cumulative": leaf.cumulative, "proof": leaf.proof })
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("serve");
    let arg = |i: usize| -> Result<&str, Error> {
        args.get(i)
            .map(String::as_str)
            .ok_or_else(|| format!("usage: dedan-aggregator {mode} DIR [WHO]").into())
    };
    match mode {
        "serve" => serve(App::new(Cfg::from_env("0.0.0.0:8060")?).await?).await,
        "countersign" => serve_countersigner(App::new(Cfg::from_env("0.0.0.0:8061")?).await?).await,
        "recompute" => recompute(Path::new(arg(2)?)),
        "report" => report(Path::new(arg(2)?)),
        "proof" => proof(Path::new(arg(2)?), arg(3)?),
        "publish" => {
            App::new(Cfg::from_env("0.0.0.0:0")?)
                .await?
                .publish_dir(Path::new(arg(2)?))
                .await
        }
        other => Err(format!("unknown mode {other}: serve | countersign | recompute DIR | report DIR | proof DIR WHO | publish DIR").into()),
    }
}
