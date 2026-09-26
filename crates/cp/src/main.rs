//! `dedan-cp`: the control plane (ARCHITECTURE.md §9, §10). It turns customer intent into signed configs and
//! runs the certificate orders nodes cannot run alone. It holds no money and no routing authority: every config
//! and purge it relays is checked again by the nodes and resolvers, and every TXT record it publishes belongs
//! to an order for a node that holds a slot.
//!
//!   GET  /v1/configs   every account's current config, signed by the control plane key (full sync)
//!   GET  /v1/stream    SSE: `config` and `purge` events with sequence ids
//!   POST /v1/purge     a `Purge` envelope signed by the account key registered on chain; pushed to the nodes
//!   POST /v1/certs     a `CertRequest` envelope signed by a node's signer key: ACME DNS-01 through the
//!                      resolvers' TXT records, the issued chain back to the node
//!
//! Accounts are seeded from the account file (a customer API and CLI are M4 items).

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::Address;
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use dedan_core::config::{self, AccountCfg, Deployment};
use dedan_core::envelope::{Envelope, KeyLookup, NonceCache};
use dedan_core::identity::Identity;
use dedan_core::types::*;
use instant_acme::{
    Account, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

type Error = Box<dyn std::error::Error + Send + Sync>;

sol! {
    #[sol(rpc)]
    interface ISettlement {
        function accounts(bytes32 id) external view returns (address key, uint256 balance, uint256 appliedBill, uint256 pendingWithdrawal, uint64 withdrawableAt);
    }
}

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
    accounts: PathBuf,
    /// The resolvers' management APIs (TXT records, the assignment), comma-separated.
    resolvers: Vec<String>,
    acme_dir: String,
    acme_root: Option<PathBuf>,
    zone: String,
}

impl Cfg {
    fn from_env() -> Self {
        let get = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
        Self {
            listen: get("DEDAN_LISTEN", "0.0.0.0:8070"),
            rpc_url: get("DEDAN_RPC_URL", "http://anvil:8545"),
            deployment: get("DEDAN_DEPLOYMENT", "/deployments/devnet.json").into(),
            accounts: get("DEDAN_ACCOUNTS", "/devnet/accounts.json").into(),
            resolvers: get("DEDAN_RESOLVERS", "http://resolver:8055")
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.trim_end_matches('/').to_owned())
                .collect(),
            acme_dir: get("DEDAN_ACME_DIR", "https://pebble:14000/dir"),
            acme_root: std::env::var("DEDAN_ACME_ROOT").ok().map(PathBuf::from),
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

struct Cp {
    cfg: Cfg,
    dep: Deployment,
    key: PrivateKeySigner,
    provider: DynProvider,
    identity: RwLock<Identity>,
    nonces: Mutex<NonceCache>,
    configs: RwLock<BTreeMap<AccountId, Envelope<AccountConfig>>>,
    seq: AtomicU64,
    events: broadcast::Sender<Event>,
    acme: Option<Account>,
    /// Orders run one at a time: the members of one account share the `_acme-challenge` name.
    acme_lock: Mutex<()>,
    http: reqwest::Client,
}

impl Cp {
    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    async fn refresh(&self) -> Result<(), Error> {
        let head = self.provider.get_block_number().await?;
        self.identity
            .write()
            .await
            .replay(&self.provider, &[self.dep.dedan_registry, self.dep.dedan_cells], head)
            .await?;
        // an account registered on chain after the seed: republish its config with the key
        let stale: Vec<AccountId> = {
            let c = self.configs.read().await;
            c.values()
                .filter(|e| e.payload.account_key == Address::ZERO)
                .map(|e| e.payload.account_id.clone())
                .collect()
        };
        for id in stale {
            let key = self.account_key(&id).await;
            if key == Address::ZERO {
                continue;
            }
            let mut c = self.configs.write().await;
            if let Some(old) = c.get(&id) {
                let seq = self.next_seq();
                let mut cfg = old.payload.clone();
                cfg.account_key = key;
                cfg.version += 1;
                cfg.seq = seq;
                let env = Envelope::sign(cfg, "cp", unix_now(), seq, &self.key)?;
                let _ = self.events.send(Event {
                    seq,
                    kind: "config",
                    body: serde_json::to_value(&env)?,
                });
                c.insert(id.clone(), env);
                info!(account = %id, %key, "account key registered on chain; config republished");
            }
        }
        Ok(())
    }

    /// The account key registered on chain with the first deposit (zero while the account does not exist).
    async fn account_key(&self, account: &AccountId) -> Address {
        ISettlement::new(self.dep.settlement, self.provider.clone())
            .accounts(account.bytes32())
            .call()
            .await
            .map(|r| r.key)
            .unwrap_or(Address::ZERO)
    }

    /// Seeds the configs from the account file: the customer intent of the devnet.
    async fn seed(&self) -> Result<(), Error> {
        let accounts = AccountCfg::load_all(&self.cfg.accounts)?;
        let mut configs = self.configs.write().await;
        for a in &accounts {
            let id = a.id();
            let seq = self.next_seq();
            let cfg = AccountConfig {
                account_id: id.clone(),
                account_key: self.account_key(&id).await,
                domains: a.hostnames.clone(),
                origins: vec![Origin {
                    url: a.origin.clone(),
                    host_header: None,
                    weight: 1,
                }],
                tier: Tier {
                    nodes: a.nodes,
                    class: a.class,
                    availability: a.availability,
                },
                cache_rules: CacheRules {
                    default_ttl_s: 600,
                    keep_query_string: true,
                    max_object_bytes: 50 << 20,
                },
                asset_list: format!("{}{}", a.origin.trim_end_matches('/'), a.asset_list),
                node_policy: NodePolicy::default(),
                version: 1,
                seq,
            };
            let env = Envelope::sign(cfg, "cp", unix_now(), seq, &self.key)?;
            let _ = self.events.send(Event {
                seq,
                kind: "config",
                body: serde_json::to_value(&env)?,
            });
            configs.insert(id, env);
        }
        info!(accounts = configs.len(), "configs published");
        Ok(())
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

    /// One ACME order for the node's SAN set: DNS-01 through the resolvers, finalised with the node's own CSR.
    async fn issue(&self, req: &CertRequest) -> Result<CertIssued, Error> {
        let acme = self.acme.as_ref().ok_or("no ACME account (DEDAN_ACME_DIR unreachable at start)")?;
        let _one_at_a_time = self.acme_lock.lock().await;
        let configs = self.configs.read().await.clone();
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
        let mut order = acme.new_order(&NewOrder::new(&idents)).await?;
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
        Ok(CertIssued {
            chain_pem,
            not_after: 0, // the node reads it from the certificate
            ari_id: None,
        })
    }
}

async fn health(State(s): State<Arc<Cp>>) -> Json<Value> {
    Json(json!({ "ok": true, "me": s.key.address(), "acme": s.acme.is_some(), "seq": s.seq.load(Ordering::Relaxed), "block": s.identity.read().await.block }))
}

async fn configs(State(s): State<Arc<Cp>>) -> Json<Value> {
    let c = s.configs.read().await;
    Json(json!({ "seq": s.seq.load(Ordering::Relaxed), "configs": c.values().collect::<Vec<_>>() }))
}

async fn stream(State(s): State<Arc<Cp>>) -> Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = s.events.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|e| e.ok()).map(|e| {
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
    let key = s.account_key(&account).await;
    struct AccountKey(String, Address);
    impl KeyLookup for AccountKey {
        fn accepts(&self, signer: &str, address: Address) -> bool {
            signer == self.0 && address == self.1 && address != Address::ZERO
        }
    }
    env.verify(&AccountKey(account.signer(), key), unix_now(), &mut *s.nonces.lock().await)
        .map_err(|e| (StatusCode::UNAUTHORIZED, format!("{e} (account key on chain: {key})")))?;
    let seq = s.next_seq();
    let body = serde_json::to_value(&env).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let _ = s.events.send(Event {
        seq,
        kind: "purge",
        body,
    });
    info!(%account, scope = ?env.payload.scope, pattern = env.payload.pattern, seq, "purge relayed");
    Ok(Json(json!({ "ok": true, "seq": seq })))
}

async fn certs(
    State(s): State<Arc<Cp>>,
    Json(env): Json<Envelope<CertRequest>>,
) -> Result<Json<CertIssued>, (StatusCode, String)> {
    if env.signer != env.payload.node_id.signer() {
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
    info!(node = %env.payload.node_id, hostnames = ?env.payload.hostnames, "certificate order");
    match s.issue(&env.payload).await {
        Ok(issued) => {
            info!(node = %env.payload.node_id, "certificate issued");
            Ok(Json(issued))
        }
        Err(e) => {
            warn!(node = %env.payload.node_id, error = %e, "certificate order failed");
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
    let cp = std::env::var("DEDAN_CP_URL").unwrap_or_else(|_| "http://127.0.0.1:8070".into());
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/purge", cp.trim_end_matches('/')))
        .json(&env)
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    println!("{status} {body}");
    if !status.is_success() {
        return Err("purge refused".into());
    }
    Ok(())
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
    if args.get(1).map(String::as_str) == Some("purge") {
        return purge_cli(&args[2..]).await;
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
    let (events, _) = broadcast::channel(1024);
    let cp = Arc::new(Cp {
        cfg,
        dep,
        key,
        provider,
        identity: RwLock::new(Identity::default()),
        nonces: Mutex::new(NonceCache::default()),
        configs: RwLock::new(BTreeMap::new()),
        seq: AtomicU64::new(0),
        events,
        acme,
        acme_lock: Mutex::new(()),
        http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?,
    });
    if let Err(e) = cp.refresh().await {
        warn!(error = %e, "first identity refresh failed");
    }
    cp.seed().await?;
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
        .route("/v1/configs", get(configs))
        .route("/v1/stream", get(stream))
        .route("/v1/purge", post(purge))
        .route("/v1/certs", post(certs))
        .with_state(cp.clone());
    let listener = tokio::net::TcpListener::bind(&cp.cfg.listen).await?;
    info!(listen = %cp.cfg.listen, me = %cp.key.address(), "control plane listening");
    axum::serve(listener, router).await?;
    Ok(())
}
