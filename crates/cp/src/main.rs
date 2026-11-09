//! `dedan-cp`: the control plane (ARCHITECTURE.md §9, §10). It relays the configs customers sign with their
//! account keys and runs the certificate orders nodes cannot run alone. It holds no money and no routing
//! authority: it writes no config, every config and purge it relays is checked again by the nodes and
//! resolvers against the account key on chain, and every TXT record it publishes belongs to an order for a
//! node that holds a slot.
//!
//!   GET  /v1/configs   every account's current config as its account key signed it: `{seq, configs}` (full sync)
//!   POST /v1/configs   an `AccountConfig` envelope signed as `account:<label>` by the account key on chain, of
//!                      a higher version than the one held (the same config again answers 200 unchanged)
//!   GET  /v1/stream    SSE: `config` and `purge` events with sequence ids; `Last-Event-ID` (or `?since=<seq>`)
//!                      replays the events after it from a log of the last 1,000, or sends one `reset` first
//!                      when the log does not reach back to it
//!   POST /v1/purge     a `Purge` envelope signed by the account key registered on chain; pushed to the nodes
//!   POST /v1/certs     a `CertRequest` envelope signed by a node's signer key (with the ARI id of the
//!                      certificate it renews, `replaces`): ACME DNS-01 through the resolvers' TXT records,
//!                      the issued chain back to the node
//!
//! The configs and the event sequence persist in `$DEDAN_CP_DATA/configs.json`. The customer's CLI:
//! `dedan-cp config <account> [accounts.json]` and `dedan-cp purge <account> <url|prefix|all> <pattern>`.

use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::Address;
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use dedan_core::config::{self, AccountCfg, Deployment};
use dedan_core::envelope::{Envelope, KeyLookup, NonceCache};
use dedan_core::identity::Identity;
use dedan_core::settlement::{self, newer, verify_config};
use dedan_core::types::*;
use instant_acme::{
    Account, AuthorizationStatus, CertificateIdentifier, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    RetryPolicy,
};
use rustls::pki_types::CertificateDer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// How many recent events the stream replays after a reconnect.
/// ponytail: the log lives in memory, so after a restart every node that was behind gets `reset` (a full sync and
/// its whole cache dropped); persist the log next to the configs if restarts become frequent.
const LOG_LEN: usize = 1_000;

mod worldid;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

struct Cfg {
    listen: String,
    rpc_url: String,
    deployment: PathBuf,
    /// Where the configs and the event sequence persist across restarts.
    data: PathBuf,
    /// The resolvers' management APIs (TXT records, the assignment), comma-separated.
    resolvers: Vec<String>,
    acme_dir: String,
    acme_root: Option<PathBuf>,
    /// The ACME profile certificates are ordered with (`shortlived`: 160 h at Let's Encrypt and in the devnet's
    /// Pebble); empty for the CA's default.
    acme_profile: Option<String>,
    zone: String,
}

impl Cfg {
    fn from_env() -> Self {
        let get = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
        Self {
            listen: get("DEDAN_LISTEN", "0.0.0.0:8070"),
            rpc_url: get("DEDAN_RPC_URL", "http://anvil:8545"),
            deployment: get("DEDAN_DEPLOYMENT", "/deployments/devnet.json").into(),
            data: get("DEDAN_CP_DATA", "/data").into(),
            resolvers: get("DEDAN_RESOLVERS", "http://resolver:8055")
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.trim_end_matches('/').to_owned())
                .collect(),
            acme_dir: get("DEDAN_ACME_DIR", "https://pebble:14000/dir"),
            acme_root: std::env::var("DEDAN_ACME_ROOT").ok().map(PathBuf::from),
            acme_profile: Some(get("DEDAN_ACME_PROFILE", "shortlived")).filter(|p| !p.is_empty()),
            zone: get("DEDAN_ZONE", "dedan-cdn.test"),
        }
    }
}

/// One event of the stream, replayable by `seq`.
#[derive(Debug, Clone, Serialize)]
struct Event {
    seq: u64,
    kind: &'static str,
    body: Value,
}

/// The accepted configs and the event log, under one lock, so a config, its event and the seq never disagree.
#[derive(Default)]
struct Store {
    seq: u64,
    configs: BTreeMap<AccountId, Envelope<AccountConfig>>,
    /// The last `LOG_LEN` events: every event after `floor` is here.
    log: VecDeque<Event>,
    floor: u64,
}

/// `configs.json`, the same shape as `GET /v1/configs`.
#[derive(Deserialize)]
struct Saved {
    seq: u64,
    configs: Vec<Envelope<AccountConfig>>,
}

impl Store {
    fn load(path: &Path) -> Result<Self, Error> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let s: Saved = serde_json::from_str(&std::fs::read_to_string(path)?)
            .map_err(|e| format!("{}: {e} (fix or remove it)", path.display()))?;
        Ok(Self {
            seq: s.seq,
            configs: s.configs.into_iter().map(|e| (e.payload.account_id.clone(), e)).collect(),
            log: VecDeque::new(),
            floor: s.seq, // the events before a restart are not replayable
        })
    }

    fn snapshot(&self) -> Value {
        json!({ "seq": self.seq, "configs": self.configs.values().collect::<Vec<_>>() })
    }

    fn push(&mut self, kind: &'static str, body: Value) -> Event {
        self.seq += 1;
        let e = Event { seq: self.seq, kind, body };
        self.log.push_back(e.clone());
        if self.log.len() > LOG_LEN {
            if let Some(old) = self.log.pop_front() {
                self.floor = old.seq;
            }
        }
        e
    }

    /// A config signed as the account by `key`, its key on chain, of a higher version than the one held;
    /// `None` when it is the same config again.
    fn accept(&mut self, env: Envelope<AccountConfig>, key: Address) -> Result<Option<Event>, (StatusCode, String)> {
        verify_config(&env, key).map_err(|e| (StatusCode::UNAUTHORIZED, e))?;
        let c = &env.payload;
        if let Some(held) = self.configs.get(&c.account_id).map(|h| &h.payload) {
            if !newer(Some(held.version), c.version) {
                if held == c {
                    return Ok(None);
                }
                let e = format!("version {} is not newer than the one held ({})", c.version, held.version);
                return Err((StatusCode::CONFLICT, e));
            }
        }
        // ponytail: first come, first served per hostname; the CNAME check of domain ownership (M4) replaces it
        if let Some(other) = self.configs.values().map(|o| &o.payload).find(|o| {
            o.account_id != c.account_id && o.domains.iter().any(|d| c.domains.iter().any(|h| h.eq_ignore_ascii_case(d)))
        }) {
            return Err((StatusCode::CONFLICT, format!("a hostname of this config is served for {}", other.account_id)));
        }
        let body = serde_json::to_value(&env).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        let event = self.push("config", body);
        self.configs.insert(env.payload.account_id.clone(), env);
        Ok(Some(event))
    }

    /// What a subscriber that last saw `since` missed: the logged events after it, or a single `reset` when the
    /// log does not reach back to it (or it is from another history, past the current seq).
    fn replay(&self, since: Option<u64>) -> Vec<Event> {
        match since {
            None => vec![],
            Some(s) if s < self.floor || s > self.seq => vec![Event {
                seq: self.seq,
                kind: "reset",
                body: json!({ "seq": self.seq }),
            }],
            Some(s) => self.log.iter().filter(|e| e.seq > s).cloned().collect(),
        }
    }
}

struct Cp {
    cfg: Cfg,
    dep: Deployment,
    key: PrivateKeySigner,
    provider: DynProvider,
    identity: RwLock<Identity>,
    nonces: Mutex<NonceCache>,
    store: std::sync::Mutex<Store>,
    events: broadcast::Sender<Event>,
    acme: Option<Account>,
    /// Orders run one at a time: the members of one account share the `_acme-challenge` name.
    acme_lock: Mutex<()>,
    http: reqwest::Client,
    /// The World ID relying party: signed IDKit requests, verified results (WORLD-ID.md).
    world_id: worldid::WorldId,
}

impl Cp {
    async fn refresh(&self) -> Result<(), Error> {
        let head = self.provider.get_block_number().await?;
        self.identity
            .write()
            .await
            .replay(&self.provider, &[self.dep.dedan_registry, self.dep.dedan_cells], head)
            .await?;
        Ok(())
    }

    /// The account key registered on chain (zero while the account does not exist).
    async fn account_key(&self, account: &AccountId) -> Result<Address, (StatusCode, String)> {
        settlement::account(&self.provider, self.dep.settlement, account, None)
            .await
            .map(|a| a.key)
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("{account} not readable on chain: {e}")))
    }

    /// Persists the configs and the seq, then sends the event to the stream; called under the store's lock, so
    /// a new subscriber's replay and its live events neither overlap nor leave a gap.
    fn emit(&self, store: &Store, event: Event) {
        let path = self.cfg.data.join("configs.json");
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, store.snapshot().to_string()).and_then(|_| std::fs::rename(&tmp, &path)) {
            error!(path = %path.display(), error = %e, "configs not persisted: a restart loses this change");
        }
        let _ = self.events.send(event);
    }

    fn account_of(&self, configs: &BTreeMap<AccountId, Envelope<AccountConfig>>, hostname: &str) -> Option<AccountId> {
        configs
            .values()
            .find(|c| c.payload.domains.iter().any(|d| d.eq_ignore_ascii_case(hostname)))
            .map(|c| c.payload.account_id.clone())
    }

    /// Where the TXT record for a name's DNS-01 challenge is served: the account's `_acme-challenge` under our
    /// zone (the customer's CNAME points there), or the node's own name.
    fn txt_host(&self, name: &str, account: Option<&AccountId>) -> String {
        match account {
            Some(a) => format!("_acme-challenge.{a}.{}", self.cfg.zone),
            None => format!("_acme-challenge.{name}"),
        }
    }

    async fn set_txt(&self, host: &str, value: &str, clear: bool) {
        for r in &self.cfg.resolvers {
            let path = if clear { "clear-txt" } else { "set-txt" };
            if let Err(e) = self
                .http
                .post(format!("{r}/{path}"))
                .json(&json!({ "host": host, "value": value }))
                .send()
                .await
            {
                warn!(resolver = r, host, error = %e, "txt record not published");
            }
        }
    }

    /// The slot sets, from a resolver's view of the public assignment.
    async fn assignment(&self) -> BTreeMap<String, Vec<String>> {
        for r in &self.cfg.resolvers {
            if let Ok(resp) = self.http.get(format!("{r}/assignment")).send().await {
                if let Ok(v) = resp.json::<Value>().await {
                    if let Some(sets) = v.get("sets").and_then(|s| s.as_object()) {
                        return sets
                            .iter()
                            .map(|(a, ns)| {
                                (
                                    a.clone(),
                                    ns.as_array()
                                        .map(|x| x.iter().filter_map(|n| n.as_str().map(str::to_owned)).collect())
                                        .unwrap_or_default(),
                                )
                            })
                            .collect();
                    }
                }
            }
        }
        BTreeMap::new()
    }

    /// One ACME order for the node's SAN set with the configured profile, naming the certificate it renews
    /// (ARI), DNS-01 through the resolvers, finalised with the node's own CSR.
    async fn issue(&self, req: &CertRequest, replaces: Option<&str>) -> Result<CertIssued, Error> {
        let acme = self.acme.as_ref().ok_or("no ACME account (DEDAN_ACME_DIR unreachable at start)")?;
        let _one_at_a_time = self.acme_lock.lock().await;
        let configs = self.store.lock().expect("store").configs.clone();
        let own = format!("{}.node.{}", req.node_id, self.cfg.zone);
        let sets = self.assignment().await;
        for h in &req.hostnames {
            if h.eq_ignore_ascii_case(&own) {
                continue;
            }
            let account = self
                .account_of(&configs, h)
                .ok_or_else(|| format!("{h} belongs to no account"))?;
            let members = sets.get(&account.0).cloned().unwrap_or_default();
            if !members.contains(&req.node_id.0) {
                return Err(format!("{} holds no slot of {account}", req.node_id).into());
            }
        }
        let idents: Vec<Identifier> = req.hostnames.iter().map(|h| Identifier::Dns(h.clone())).collect();
        let profile = self.cfg.acme_profile.as_deref();
        let renewed = replaces.and_then(|r| r.split_once('.')).map(|(aki, serial)| CertificateIdentifier {
            authority_key_identifier: aki.into(),
            serial: serial.into(),
        });
        let mut order = match renewed {
            // the CA refuses a replacement it cannot match (a certificate of the ACME account before a restart,
            // unknown, or already replaced): a plain order then
            Some(id) => match acme.new_order(&new_order(&idents, profile).replaces(id)).await {
                Ok(o) => o,
                Err(e) => {
                    warn!(node = %req.node_id, error = %e, "ARI replacement refused; ordering without `replaces`");
                    acme.new_order(&new_order(&idents, profile)).await?
                }
            },
            None => acme.new_order(&new_order(&idents, profile)).await?,
        };
        let mut published: Vec<(String, String)> = Vec::new();
        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut authz = result?;
            if authz.status == AuthorizationStatus::Valid {
                continue;
            }
            let mut challenge = authz
                .challenge(ChallengeType::Dns01)
                .ok_or("the CA offers no dns-01 challenge")?;
            let name = challenge.identifier().to_string();
            let account = self.account_of(&configs, &name);
            let host = self.txt_host(&name, account.as_ref());
            let value = challenge.key_authorization().dns_value();
            self.set_txt(&host, &value, false).await;
            published.push((host, value));
            challenge.set_ready().await?;
        }
        let status = order.poll_ready(&RetryPolicy::default()).await;
        for (host, value) in &published {
            self.set_txt(host, value, true).await;
        }
        if status? != OrderStatus::Ready {
            return Err("the order did not become ready".into());
        }
        let csr = pem::parse(&req.csr_pem)?;
        order.finalize_csr(csr.contents()).await?;
        let chain_pem = order.poll_certificate(&RetryPolicy::default()).await?;
        // the leaf's ARI identifier: the node names it as `replaces` when it renews this certificate
        let ari_id = pem::parse_many(&chain_pem)?
            .first()
            .and_then(|leaf| CertificateIdentifier::try_from(&CertificateDer::from(leaf.contents().to_vec())).ok())
            .map(|id| id.to_string());
        Ok(CertIssued {
            chain_pem,
            not_after: 0, // the node reads it from the certificate
            ari_id,
        })
    }
}

/// An order for `idents` with the ACME profile, if one is configured.
fn new_order<'a>(idents: &'a [Identifier], profile: Option<&'a str>) -> NewOrder<'a> {
    match profile {
        Some(p) => NewOrder::new(idents).profile(p),
        None => NewOrder::new(idents),
    }
}

async fn health(State(s): State<Arc<Cp>>) -> Json<Value> {
    let seq = s.store.lock().expect("store").seq;
    Json(json!({ "ok": true, "me": s.key.address(), "acme": s.acme.is_some(), "seq": seq, "block": s.identity.read().await.block }))
}

async fn configs(State(s): State<Arc<Cp>>) -> Json<Value> {
    Json(s.store.lock().expect("store").snapshot())
}

/// A config the customer signed with its account key: verified against the key on chain, kept when newer,
/// persisted and pushed to the stream.
async fn post_config(
    State(s): State<Arc<Cp>>,
    Json(env): Json<Envelope<AccountConfig>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (account, version) = (env.payload.account_id.clone(), env.payload.version);
    let key = s.account_key(&account).await?;
    let mut store = s.store.lock().expect("store");
    let accepted = store.accept(env, key).inspect_err(|(status, e)| {
        warn!(%account, version, %status, error = e, "config refused");
    })?;
    let Some(event) = accepted else {
        return Ok(Json(json!({ "ok": true, "unchanged": true, "version": version })));
    };
    let seq = event.seq;
    s.emit(&store, event);
    info!(%account, version, seq, %key, "config accepted");
    Ok(Json(json!({ "ok": true, "seq": seq, "version": version, "signer": account.signer(), "key": key })))
}

#[derive(Deserialize)]
struct Since {
    since: Option<u64>,
}

/// The events after the subscriber's last one (`Last-Event-ID`, or `?since=`) from the log, then live ones.
async fn stream(
    State(s): State<Arc<Cp>>,
    headers: HeaderMap,
    Query(q): Query<Since>,
) -> Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>> {
    let since = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok()?.trim().parse().ok())
        .or(q.since);
    // subscribed under the store's lock: every event is either in the replay or on the receiver, once
    let (missed, rx) = {
        let store = s.store.lock().expect("store");
        (store.replay(since), s.events.subscribe())
    };
    if missed.first().is_some_and(|e| e.kind == "reset") {
        info!(?since, "stream: the event log does not reach back; reset sent");
    }
    // a subscriber that falls behind the channel is cut off; it reconnects and replays from the log
    let live = BroadcastStream::new(rx).map_while(Result::ok);
    let stream = tokio_stream::iter(missed).chain(live).map(|e| {
        Ok(SseEvent::default()
            .id(e.seq.to_string())
            .event(e.kind)
            .data(e.body.to_string()))
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn purge(
    State(s): State<Arc<Cp>>,
    Json(env): Json<Envelope<Purge>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let account = env.payload.account_id.clone();
    if env.signer != account.signer() {
        return Err((StatusCode::BAD_REQUEST, "signer is not the account".into()));
    }
    let key = s.account_key(&account).await?;
    struct AccountKey(String, Address);
    impl KeyLookup for AccountKey {
        fn accepts(&self, signer: &str, address: Address) -> bool {
            signer == self.0 && address == self.1 && address != Address::ZERO
        }
    }
    env.verify(&AccountKey(account.signer(), key), unix_now(), &mut *s.nonces.lock().await)
        .map_err(|e| (StatusCode::UNAUTHORIZED, format!("{e} (account key on chain: {key})")))?;
    let body = serde_json::to_value(&env).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let seq = {
        let mut store = s.store.lock().expect("store");
        let event = store.push("purge", body);
        let seq = event.seq;
        s.emit(&store, event);
        seq
    };
    info!(%account, scope = ?env.payload.scope, pattern = env.payload.pattern, seq, "purge relayed");
    Ok(Json(json!({ "ok": true, "seq": seq })))
}

async fn certs(
    State(s): State<Arc<Cp>>,
    Json(env): Json<Envelope<CertRequest>>,
) -> Result<Json<CertIssued>, (StatusCode, String)> {
    let req = &env.payload;
    if env.signer != req.node_id.signer() {
        return Err((StatusCode::BAD_REQUEST, "signer is not the node".into()));
    }
    let res = {
        let id = s.identity.read().await;
        env.verify(&*id, unix_now(), &mut *s.nonces.lock().await)
    };
    if res.is_err() {
        let _ = s.refresh().await; // a signer granted a moment ago
        let id = s.identity.read().await;
        env.verify(&*id, unix_now(), &mut *s.nonces.lock().await)
            .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;
    }
    let replaces = env.payload.replaces.as_deref();
    info!(node = %req.node_id, hostnames = ?req.hostnames, profile = ?s.cfg.acme_profile, replaces, "certificate order");
    match s.issue(req, replaces).await {
        Ok(issued) => {
            info!(node = %req.node_id, ari_id = issued.ari_id, "certificate issued");
            Ok(Json(issued))
        }
        Err(e) => {
            warn!(node = %req.node_id, error = %e, "certificate order failed");
            Err((StatusCode::BAD_GATEWAY, e.to_string()))
        }
    }
}

async fn acme_account(cfg: &Cfg) -> Result<Account, Error> {
    let builder = match &cfg.acme_root {
        Some(root) => Account::builder_with_root(root)?,
        None => Account::builder()?,
    };
    let (account, _credentials) = builder
        .create(
            &NewAccount {
                contact: &[],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            cfg.acme_dir.clone(),
            None,
        )
        .await?;
    Ok(account)
}

/// Posts a signed envelope to the control plane at `DEDAN_CP_URL` and prints its answer.
async fn post_cli<T: Serialize>(path: &str, env: &Envelope<T>) -> Result<(), Error> {
    let cp = std::env::var("DEDAN_CP_URL").unwrap_or_else(|_| "http://127.0.0.1:8070".into());
    let resp = reqwest::Client::new()
        .post(format!("{}{path}", cp.trim_end_matches('/')))
        .json(env)
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    println!("{status} {body}");
    if !status.is_success() {
        return Err(format!("{path} refused").into());
    }
    Ok(())
}

/// `dedan-cp config <account> [accounts.json]`: the account's config from the account file (default
/// `DEDAN_ACCOUNTS`) as a new version (unix time), signed as the account by the key in the environment (the
/// customer's account key), posted to the control plane. What a customer's CLI does.
async fn config_cli(args: &[String]) -> Result<(), Error> {
    let account = args.first().ok_or("usage: dedan-cp config <account> [accounts.json]")?;
    let file = match args.get(1) {
        Some(f) => f.clone(),
        None => std::env::var("DEDAN_ACCOUNTS").unwrap_or_else(|_| "/devnet/accounts.json".into()),
    };
    let a = AccountCfg::load_all(&file)?
        .into_iter()
        .find(|a| &a.label == account)
        .ok_or_else(|| format!("{account} is not in {file}"))?;
    let key = config::signer_from_env()?;
    let now = unix_now();
    let mut cfg = a.to_config(now);
    cfg.account_key = key.address(); // the key it is signed with, for readers; they check it on chain
    let env = Envelope::sign(cfg, a.id().signer(), now, now, &key)?;
    println!("config {account} v{now} signed as {} by {}", env.signer, key.address());
    post_cli("/v1/configs", &env).await
}

/// `dedan-cp purge <account> <url|prefix|all> <pattern>`: a purge signed by the key in the environment (the
/// customer's account key), posted to the control plane at `DEDAN_CP_URL`. What a customer's CLI does.
async fn purge_cli(args: &[String]) -> Result<(), Error> {
    let (account, scope, pattern) = (
        args.first().ok_or("account")?,
        args.get(1).map(String::as_str).unwrap_or("all"),
        args.get(2).map(String::as_str).unwrap_or("*"),
    );
    let key = config::signer_from_env()?;
    let scope = match scope {
        "url" => PurgeScope::Url,
        "prefix" => PurgeScope::Prefix,
        _ => PurgeScope::All,
    };
    let p = Purge {
        account_id: AccountId::new(account),
        seq: unix_now(),
        scope,
        pattern: pattern.to_owned(),
    };
    let env = Envelope::sign(p, AccountId::new(account).signer(), unix_now(), unix_now(), &key)?;
    post_cli("/v1/purge", &env).await
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .init();
    // two rustls providers are compiled into the workspace (Pingora's ring, instant-acme's aws-lc-rs): pick one
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("purge") => return purge_cli(&args[2..]).await,
        Some("config") => return config_cli(&args[2..]).await,
        _ => {}
    }
    let cfg = Cfg::from_env();
    let key = config::signer_from_env()?;
    let dep = Deployment::load(&cfg.deployment)?;
    let provider = ProviderBuilder::new().connect_http(cfg.rpc_url.parse()?).erased();
    let mut acme = None;
    for attempt in 1..=10 {
        match acme_account(&cfg).await {
            Ok(a) => {
                info!(dir = cfg.acme_dir, "ACME account created");
                acme = Some(a);
                break;
            }
            Err(e) => {
                warn!(attempt, error = %e, "ACME directory not ready");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
    if let Err(e) = std::fs::create_dir_all(&cfg.data) {
        warn!(data = %cfg.data.display(), error = %e, "no data directory: configs will not survive a restart");
    }
    let store = Store::load(&cfg.data.join("configs.json"))?;
    info!(configs = store.configs.len(), seq = store.seq, "configs loaded");
    let (events, _) = broadcast::channel(1024);
    let cp = Arc::new(Cp {
        cfg,
        dep,
        key,
        provider,
        identity: RwLock::new(Identity::default()),
        nonces: Mutex::new(NonceCache::default()),
        store: std::sync::Mutex::new(store),
        events,
        acme,
        acme_lock: Mutex::new(()),
        http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?,
        world_id: worldid::WorldId::from_env(),
    });
    if let Err(e) = cp.refresh().await {
        warn!(error = %e, "first identity refresh failed");
    }
    let sync = cp.clone();
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
        .route("/v1/configs", get(configs).post(post_config))
        .route("/v1/stream", get(stream))
        .route("/v1/purge", post(purge))
        .route("/v1/certs", post(certs))
        .route("/v1/world-id/config", get(worldid::config))
        .route("/v1/world-id/request", post(worldid::request))
        .route("/v1/world-id/verify", post(worldid::verify))
        .with_state(cp.clone());
    let listener = tokio::net::TcpListener::bind(&cp.cfg.listen).await?;
    info!(listen = %cp.cfg.listen, me = %cp.key.address(), "control plane listening");
    axum::serve(listener, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acme(version: u64) -> AccountConfig {
        let file = r#"{"label":"acme","hostnames":["www.images.com"],"nodes":6,"class":"100M","availability":"99.9","origin":"http://origin"}"#;
        serde_json::from_str::<AccountCfg>(file).unwrap().to_config(version)
    }

    #[test]
    fn only_newer_configs_signed_by_the_account_key_are_accepted() {
        let customer = PrivateKeySigner::from_slice(&[1u8; 32]).unwrap();
        let cp = PrivateKeySigner::from_slice(&[2u8; 32]).unwrap();
        let key = customer.address();
        let signed = |v: u64, signer: &str, k: &PrivateKeySigner| Envelope::sign(acme(v), signer, 1, v, k).unwrap();
        let mut s = Store::default();
        let refused = |r: Result<Option<Event>, (StatusCode, String)>| r.unwrap_err().0;
        assert_eq!(refused(s.accept(signed(5, "cp", &cp), key)), StatusCode::UNAUTHORIZED, "the cp cannot write one");
        assert_eq!(refused(s.accept(signed(5, "account:acme", &cp), key)), StatusCode::UNAUTHORIZED, "another key");
        assert_eq!(s.accept(signed(5, "account:acme", &customer), key).unwrap().unwrap().seq, 1);
        assert!(s.accept(signed(5, "account:acme", &customer), key).unwrap().is_none(), "the same config again");
        assert_eq!(refused(s.accept(signed(4, "account:acme", &customer), key)), StatusCode::CONFLICT, "a replay");
        assert_eq!(s.accept(signed(6, "account:acme", &customer), key).unwrap().unwrap().seq, 2);
        assert_eq!(s.configs[&AccountId::new("acme")].payload.version, 6);
        let mut other = acme(1);
        other.account_id = AccountId::new("evil");
        let other = Envelope::sign(other, "account:evil", 1, 1, &cp).unwrap();
        assert_eq!(refused(s.accept(other, cp.address())), StatusCode::CONFLICT, "acme's hostname");
    }

    #[test]
    fn a_cert_order_verifies_with_and_without_replaces() {
        let node = PrivateKeySigner::from_slice(&[3u8; 32]).unwrap();
        let req = CertRequest {
            node_id: NodeId::new("fra"),
            account_id: AccountId::new("acme"),
            hostnames: vec!["www.images.com".into()],
            csr_pem: "csr".into(),
            replaces: None,
        };
        let wire = |text: String| serde_json::from_str::<Envelope<CertRequest>>(&text).unwrap();
        let plain = wire(serde_json::to_string(&Envelope::sign(req.clone(), "node:fra", 1, 1, &node).unwrap()).unwrap());
        assert_eq!(plain.recover().unwrap(), node.address(), "a first order carries no `replaces`");
        assert!(!serde_json::to_string(&plain.payload).unwrap().contains("replaces"));
        let order = CertRequest {
            replaces: Some("aki.serial".into()),
            ..req
        };
        let renewal = wire(serde_json::to_string(&Envelope::sign(order, "node:fra", 1, 2, &node).unwrap()).unwrap());
        assert_eq!(renewal.recover().unwrap(), node.address());
        assert_eq!(renewal.payload.replaces.as_deref(), Some("aki.serial"));
    }

    #[test]
    fn the_log_replays_after_a_seq_and_resets_when_it_does_not_reach_back() {
        let mut s = Store::default();
        for _ in 0..LOG_LEN + 5 {
            s.push("purge", json!({}));
        }
        let kinds = |v: Vec<Event>| v.iter().map(|e| (e.kind, e.seq)).collect::<Vec<_>>();
        assert_eq!(kinds(s.replay(Some(1003))), [("purge", 1004), ("purge", 1005)]);
        assert_eq!(s.replay(Some(5)).len(), LOG_LEN, "the whole log");
        assert_eq!(kinds(s.replay(Some(4))), [("reset", 1005)], "event 5 fell out of the log");
        assert_eq!(kinds(s.replay(Some(2000))), [("reset", 1005)], "another history");
        assert!(s.replay(Some(1005)).is_empty() && s.replay(None).is_empty());
        // a restart keeps the seq, but not the log
        let path = std::env::temp_dir().join(format!("dedan-cp-test-{}.json", std::process::id()));
        std::fs::write(&path, s.snapshot().to_string()).unwrap();
        let mut back = Store::load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(back.replay(Some(1005)).is_empty());
        assert_eq!(kinds(back.replay(Some(1004))), [("reset", 1005)]);
        assert_eq!(back.push("purge", json!({})).seq, 1006);
    }
}
