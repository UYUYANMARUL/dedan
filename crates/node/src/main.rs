//! `dedan-node`: a Dedan CDN node (ARCHITECTURE.md §5). One Pingora reverse proxy with a cache in front of
//! each account's origin, routing by Host, refusing unknown hosts with 421, minting the fetch token on every
//! response with a random hourly key that its hot key reveals on chain, heartbeating to the resolvers, and
//! serving the class blob and the endpoint challenge under `/.well-known/dedan/`.
//!
//! Accounts come from the control plane's signed configs (full sync at start, then its event stream; the
//! account file when no control plane is configured); purges bump the account's cache generation; a
//! certificate per SAN set is obtained through the control plane and served per SNI (`tls`).
//! ponytail: the per-account rate cap is the one §5 item still missing.

mod control;
mod proxy;
mod tls;

use std::collections::{BTreeMap, HashMap};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use alloy::sol;
use async_trait::async_trait;
use dedan_core::config::{self, AccountCfg, Deployment};
use dedan_core::envelope::Envelope;
use dedan_core::types::*;
use pingora::listeners::tls::TlsSettings;
use pingora::listeners::TlsAccept;
use pingora::server::{configuration::Opt, Server, ShutdownWatch};
use pingora::services::background::{background_service, BackgroundService};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

type Error = Box<dyn std::error::Error + Send + Sync>;

sol! {
    #[sol(rpc)]
    interface IDedanRegistry {
        function revealKey(string label, uint64 hour, bytes32 kHour) external;
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where a Host goes: the account, its origin (address, TLS, Host header) and the cache generation.
#[derive(Debug, Clone)]
pub struct Route {
    pub account: String,
    pub origin_addr: String,
    pub origin_tls: bool,
    pub origin_host: String,
    pub generation: u64,
}

/// The hourly token keys: random, kept for the current and the next hour, revealed on chain after the hour
/// (or at once on the devnet, `DEDAN_REVEAL_IMMEDIATELY`, so tokens verify without waiting an hour).
pub struct Keys {
    keys: Mutex<BTreeMap<u64, [u8; 32]>>,
    revealed: Mutex<BTreeMap<u64, bool>>,
}

impl Keys {
    fn new() -> Self {
        Self {
            keys: Mutex::new(BTreeMap::new()),
            revealed: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn key_for(&self, hour: u64) -> [u8; 32] {
        let mut keys = self.keys.lock().expect("keys");
        *keys
            .entry(hour)
            .or_insert_with(rand::random::<[u8; 32]>)
    }

    fn pending_reveals(&self, hour_now: u64, immediately: bool) -> Vec<(u64, [u8; 32])> {
        let keys = self.keys.lock().expect("keys");
        let revealed = self.revealed.lock().expect("revealed");
        keys.iter()
            .filter(|(h, _)| !revealed.get(h).copied().unwrap_or(false))
            .filter(|(h, _)| immediately || **h < hour_now)
            .map(|(h, k)| (*h, *k))
            .collect()
    }

    fn mark_revealed(&self, hour: u64) {
        self.revealed.lock().expect("revealed").insert(hour, true);
        // keys older than two hours are of no further use
        self.keys.lock().expect("keys").retain(|h, _| *h + 2 >= hour);
    }
}

pub struct Node {
    pub label: NodeId,
    pub class: Class,
    pub version: String,
    pub zone: String,
    pub keys: Keys,
    pub certs: Arc<tls::CertStore>,
    routes: RwLock<HashMap<String, Route>>,
    /// The cache generation per account: a purge bumps it and every key of the account changes.
    generations: RwLock<HashMap<String, u64>>,
    /// The account keys from the configs, what purges must be signed with.
    account_keys: RwLock<HashMap<String, Address>>,
    key: PrivateKeySigner,
    provider: DynProvider,
    dep: Deployment,
    resolvers: Vec<String>,
    cp_url: Option<String>,
    origin_override: Option<String>,
    reveal_immediately: bool,
    nonce: AtomicU64,
    http: reqwest::Client,
}

impl Node {
    pub fn route(&self, host: &str) -> Option<Route> {
        let mut r = self.routes.read().expect("routes").get(host).cloned()?;
        r.generation = self
            .generations
            .read()
            .expect("generations")
            .get(&r.account)
            .copied()
            .unwrap_or(0);
        Some(r)
    }

    pub fn signer_address(&self) -> Address {
        self.key.address()
    }

    /// The endpoint challenge: the nonce signed by this node's hot key (EIP-191), proving the address
    /// answering for the name is run by a holder of its `ROLE_NODE_SIGNER`.
    pub fn sign_challenge(&self, nonce: &str) -> Result<String, Error> {
        let sig = self.key.sign_message_sync(nonce.as_bytes())?;
        Ok(format!("0x{}", hex::encode(sig.as_bytes())))
    }

    async fn reveal_due(&self) {
        let hour = unix_now() / 3600;
        // make sure this hour's and the next hour's keys exist before anything is revealed
        self.keys.key_for(hour);
        self.keys.key_for(hour + 1);
        for (h, k) in self.keys.pending_reveals(hour, self.reveal_immediately) {
            let reg = IDedanRegistry::new(self.dep.dedan_registry, self.provider.clone());
            match reg
                .revealKey(self.label.0.clone(), h, B256::from(k))
                .send()
                .await
            {
                Ok(pending) => match pending.get_receipt().await {
                    Ok(r) if r.status() => {
                        self.keys.mark_revealed(h);
                        info!(hour = h, tx = %r.transaction_hash, "token key revealed on chain");
                    }
                    Ok(_) => warn!(hour = h, "revealKey reverted"),
                    Err(e) => warn!(hour = h, error = %e, "revealKey receipt"),
                },
                Err(e) => warn!(hour = h, error = %e, "revealKey failed"),
            }
        }
    }

    async fn heartbeat(&self) {
        let now = unix_now();
        let hb = Heartbeat {
            node_id: self.label.clone(),
            classes: vec![self.class],
            version: self.version.clone(),
            drain: false,
            load_mbps: 0, // ponytail: the rate cap's counters feed this once it exists
            connections: 0,
            cpu_pct: 0,
        };
        let env = match Envelope::sign(
            hb,
            self.label.signer(),
            now,
            self.nonce.fetch_add(1, Ordering::Relaxed),
            &self.key,
        ) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "heartbeat envelope");
                return;
            }
        };
        for r in &self.resolvers {
            let res = self
                .http
                .post(format!("{}/heartbeat", r.trim_end_matches('/')))
                .json(&env)
                .send()
                .await;
            match res {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => warn!(resolver = r, status = %resp.status(), "heartbeat refused"),
                Err(e) => warn!(resolver = r, error = %e, "heartbeat not delivered"),
            }
        }
    }
}

/// The node's loops: keys and reveals, heartbeats every 10 s, the control plane's configs and stream, the
/// certificate loop every minute.
struct Loops(Arc<Node>);

#[async_trait]
impl BackgroundService for Loops {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        let node = self.0.clone();
        let reveals = tokio::spawn(async move {
            loop {
                node.reveal_due().await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
        let node = self.0.clone();
        let beats = tokio::spawn(async move {
            loop {
                node.heartbeat().await;
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        });
        let node = self.0.clone();
        let syncs = tokio::spawn(async move {
            loop {
                match node.full_sync().await {
                    Ok(n) if n > 0 => info!(configs = n, "control plane full sync"),
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "control plane full sync failed"),
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
        let stream = tokio::spawn(self.0.clone().stream_events());
        let node = self.0.clone();
        let certs = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await; // after the first config sync
            loop {
                node.renew_certificates().await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
        let _ = shutdown.changed().await;
        for t in [reveals, beats, syncs, stream, certs] {
            t.abort();
        }
    }
}

/// The TLS listener takes its certificates from the store per SNI; nothing else to hook.
struct NoTlsHooks;

#[async_trait]
impl TlsAccept for NoTlsHooks {}

/// `http://origin` → ("origin:80", false, "origin"); `https://cdn.example.com:8443/` → tls.
pub fn parse_origin(url: &str) -> Result<(String, bool, String), Error> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("origin url needs a scheme")?;
    let tls = scheme == "https";
    let hostport = rest.split('/').next().unwrap_or("");
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h.to_owned(), p.parse::<u16>()?),
        _ => (hostport.to_owned(), if tls { 443 } else { 80 }),
    };
    Ok((format!("{host}:{port}"), tls, host))
}

fn routes_from(accounts: &[AccountCfg], origin_override: Option<&str>) -> Result<HashMap<String, Route>, Error> {
    let mut routes = HashMap::new();
    for a in accounts {
        let origin = origin_override.unwrap_or(&a.origin);
        if origin.is_empty() {
            continue;
        }
        let (addr, tls, host) = parse_origin(origin)?;
        for h in &a.hostnames {
            routes.insert(
                h.to_lowercase(),
                Route {
                    account: a.label.clone(),
                    origin_addr: addr.clone(),
                    origin_tls: tls,
                    origin_host: host.clone(),
                    generation: 0,
                },
            );
        }
    }
    Ok(routes)
}

fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let get = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
    let label = NodeId::new(std::env::var("NODE_ID").map_err(|_| "NODE_ID is required")?);
    let listen = get("DEDAN_LISTEN", "0.0.0.0:80");
    let listen_tls = get("DEDAN_LISTEN_TLS", "0.0.0.0:443");
    let cp_url = std::env::var("DEDAN_CP_URL").ok().filter(|s| !s.is_empty()).map(|s| s.trim_end_matches('/').to_owned());
    let origin_override = std::env::var("DEDAN_ORIGIN_OVERRIDE").ok();
    // the account file seeds the routes until the control plane answers (and is all there is without one)
    let accounts = AccountCfg::load_all(PathBuf::from(get("DEDAN_ACCOUNTS", "/devnet/accounts.json"))).unwrap_or_default();
    let routes = routes_from(&accounts, origin_override.as_deref())?;
    let key = config::signer_from_env()?;
    let dep = Deployment::load(get("DEDAN_DEPLOYMENT", "/deployments/devnet.json"))?;
    let rpc: reqwest::Url = get("DEDAN_RPC_URL", "http://anvil:8545").parse()?;
    let resolvers: Vec<String> = get("DEDAN_RESOLVERS", "http://10.77.0.53:8055")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    // the listen address must be a socket address for Pingora
    let _: SocketAddr = listen
        .to_socket_addrs()?
        .next()
        .ok_or("DEDAN_LISTEN")?;

    let node = Arc::new(Node {
        class: Class::parse_record(&get("DEDAN_CLASS", "100M")).unwrap_or(Class::M100),
        version: format!("dedan-node {}", env!("CARGO_PKG_VERSION")),
        zone: get("DEDAN_ZONE", "dedan-cdn.test"),
        keys: Keys::new(),
        certs: Arc::new(tls::CertStore::default()),
        routes: RwLock::new(routes),
        generations: RwLock::new(HashMap::new()),
        account_keys: RwLock::new(HashMap::new()),
        provider: ProviderBuilder::new().wallet(key.clone()).connect_http(rpc).erased(),
        key,
        dep,
        resolvers,
        cp_url,
        origin_override,
        reveal_immediately: get("DEDAN_REVEAL_IMMEDIATELY", "0") == "1",
        nonce: AtomicU64::new(unix_now()),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(45)) // the event stream: a keep-alive every 15 s; a dead cp shows within 45 s
            .build()?,
        label,
    });
    info!(
        label = %node.label,
        listen,
        listen_tls,
        hosts = ?node.routes.read().expect("routes").keys().collect::<Vec<_>>(),
        signer = %node.signer_address(),
        resolvers = ?node.resolvers,
        control_plane = ?node.cp_url,
        reveal_immediately = node.reveal_immediately,
        "starting"
    );

    let mut server = Server::new(Opt::default())?;
    server.bootstrap();
    let mut service = pingora::proxy::http_proxy_service(&server.configuration, proxy::Proxy(node.clone()));
    service.add_tcp(&listen);
    let mut tls_settings = TlsSettings::with_callbacks(Box::new(NoTlsHooks))?;
    tls_settings.set_cert_resolver(node.certs.clone());
    tls_settings.enable_h2();
    service.add_tls_with_settings(&listen_tls, None, tls_settings);
    server.add_service(service);
    server.add_service(background_service("node loops", Loops(node)));
    server.run_forever()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_parse() {
        assert_eq!(parse_origin("http://origin").unwrap(), ("origin:80".into(), false, "origin".into()));
        assert_eq!(
            parse_origin("https://cdn.example.com:8443/x").unwrap(),
            ("cdn.example.com:8443".into(), true, "cdn.example.com".into())
        );
        assert!(parse_origin("origin").is_err());
    }

    #[test]
    fn keys_reveal_after_the_hour_unless_told_otherwise() {
        let k = Keys::new();
        let a = k.key_for(10);
        assert_eq!(k.key_for(10), a, "stable within the hour");
        k.key_for(11);
        assert_eq!(k.pending_reveals(10, false), vec![], "nothing ended yet");
        assert_eq!(k.pending_reveals(11, false).len(), 1, "hour 10 ended");
        assert_eq!(k.pending_reveals(10, true).len(), 2, "devnet: at once");
        k.mark_revealed(10);
        assert_eq!(k.pending_reveals(11, false), vec![]);
    }
}
