//! The request path (ARCHITECTURE.md §4, §5): Host → account → origin, the cache, the fetch token on every
//! response, and the node's own endpoints (`/.well-known/dedan/blob/<n>`, `/.well-known/dedan/challenge/<nonce>`).

use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use dedan_core::token;
use pingora::cache::cache_control::CacheControl;
use pingora::cache::eviction::simple_lru::Manager;
use pingora::cache::filters::resp_cacheable;
use pingora::cache::lock::CacheLock;
use pingora::cache::predictor::Predictor;
use pingora::cache::{CacheKey, CacheMetaDefaults, CachePhase, MemCache, RespCacheable};
use pingora::http::{ResponseHeader, StatusCode};
use pingora::prelude::*;
use pingora::protocols::http::error_resp;
use serde_json::json;
use tracing::{debug, info};

use crate::{unix_now, Node};

/// The cache: in memory in v1, bounded by an LRU, single-flight per key.
/// ponytail: `pingora-cache`'s `MemCache`; the disk `Storage` (file per key, atomic rename) is the next tier.
static STORAGE: LazyLock<MemCache> = LazyLock::new(MemCache::new);
static EVICTION: LazyLock<Manager> = LazyLock::new(|| Manager::new(cache_bytes()));
static PREDICTOR: LazyLock<Predictor<32>> = LazyLock::new(|| Predictor::new(128, None));
static LOCK: LazyLock<Box<CacheLock>> = LazyLock::new(|| CacheLock::new_boxed(Duration::from_secs(5)));
/// Default freshness for cacheable statuses when the origin says nothing; stale-while-revalidate 30 s,
/// stale-if-error 24 h (§5: serve stale on origin failure for up to 24 h).
static META_DEFAULTS: LazyLock<CacheMetaDefaults> = LazyLock::new(|| {
    CacheMetaDefaults::new(
        |status| match status.as_u16() {
            200 | 301 | 302 | 404 => Some(Duration::from_secs(default_ttl())),
            _ => None,
        },
        30,
        86_400,
    )
});

fn cache_bytes() -> usize {
    std::env::var("DEDAN_CACHE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(512)
        << 20
}

fn default_ttl() -> u64 {
    std::env::var("DEDAN_DEFAULT_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600)
}

pub const BLOB_PREFIX: &str = "/.well-known/dedan/blob/";
pub const CHALLENGE_PREFIX: &str = "/.well-known/dedan/challenge/";
const MB: usize = 1 << 20;

pub struct Ctx {
    /// The account the Host belongs to, and its origin, once routed.
    account: Option<crate::Route>,
}

pub struct Proxy(pub std::sync::Arc<Node>);

impl Proxy {
    fn node(&self) -> &Node {
        &self.0
    }

    /// The Host header (HTTP/1.1) or the `:authority` (HTTP/2), without the port.
    fn host_of(session: &Session) -> Option<String> {
        let h = session.req_header();
        let raw = h
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .or_else(|| h.uri.authority().map(|a| a.to_string()))?;
        Some(raw.split(':').next().unwrap_or("").trim().to_lowercase())
    }

    fn token_for(&self, session: &Session) -> String {
        let ip = session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map(|a| a.ip())
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        let minute = token::minute_of(unix_now());
        let k = self.node().keys.key_for(minute / 60);
        token::mint(&self.node().label, minute, &k, &token::prefix(ip)).to_string()
    }

    async fn serve_blob(&self, session: &mut Session, n: usize) -> pingora::Result<()> {
        let n = n.clamp(1, 64);
        let mut resp = ResponseHeader::build(200, Some(4))?;
        resp.insert_header("content-type", "application/octet-stream")?;
        resp.insert_header("content-length", (n * MB).to_string())?;
        resp.insert_header("cache-control", "no-store")?;
        resp.insert_header(token::HEADER, self.token_for(session))?;
        session.write_response_header(Box::new(resp), false).await?;
        // deterministic bytes: every probe of the same size downloads the same body, so auditors can hash it
        let chunk: Bytes = (0..MB)
            .map(|i| ((i as u64).wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        for i in 0..n {
            session
                .write_response_body(Some(chunk.clone()), i + 1 == n)
                .await?;
        }
        Ok(())
    }

    async fn serve_challenge(&self, session: &mut Session, nonce: &str) -> pingora::Result<()> {
        let body = match self.node().sign_challenge(nonce) {
            Ok(sig) => json!({
                "label": self.node().label, "nonce": nonce, "signer": self.node().signer_address(), "sig": sig,
            })
            .to_string(),
            Err(e) => json!({ "error": e.to_string() }).to_string(),
        };
        let mut resp = ResponseHeader::build(200, Some(4))?;
        resp.insert_header("content-type", "application/json")?;
        resp.insert_header("content-length", body.len().to_string())?;
        resp.insert_header("cache-control", "no-store")?;
        resp.insert_header(token::HEADER, self.token_for(session))?;
        session.write_response_header(Box::new(resp), false).await?;
        session
            .write_response_body(Some(Bytes::from(body)), true)
            .await?;
        Ok(())
    }

    async fn respond(&self, session: &mut Session, code: u16) -> pingora::Result<()> {
        let mut resp = error_resp::gen_error_response(code);
        resp.insert_header(token::HEADER, self.token_for(session))?;
        session.write_response_header(Box::new(resp), false).await?;
        session.write_response_body(None, true).await?;
        Ok(())
    }
}

#[async_trait]
impl ProxyHttp for Proxy {
    type CTX = Ctx;

    fn new_ctx(&self) -> Ctx {
        Ctx { account: None }
    }

    /// The node's own endpoints are answered here; everything else is routed by Host or refused with 421.
    async fn request_filter(&self, session: &mut Session, ctx: &mut Ctx) -> pingora::Result<bool> {
        let path = session.req_header().uri.path().to_owned();
        if let Some(rest) = path.strip_prefix(BLOB_PREFIX) {
            let n: usize = rest.trim_end_matches('/').parse().unwrap_or(0);
            if n == 0 {
                self.respond(session, 404).await?;
                return Ok(true);
            }
            self.serve_blob(session, n).await?;
            return Ok(true);
        }
        if let Some(nonce) = path.strip_prefix(CHALLENGE_PREFIX) {
            self.serve_challenge(session, nonce.trim_end_matches('/'))
                .await?;
            return Ok(true);
        }
        let Some(host) = Self::host_of(session) else {
            self.respond(session, 400).await?;
            return Ok(true);
        };
        match self.node().route(&host) {
            Some(route) => {
                ctx.account = Some(route);
                Ok(false)
            }
            None => {
                debug!(host, "unknown host: 421");
                self.respond(session, 421).await?;
                Ok(true)
            }
        }
    }

    async fn upstream_peer(&self, _session: &mut Session, ctx: &mut Ctx) -> pingora::Result<Box<HttpPeer>> {
        let route = ctx.account.as_ref().expect("routed in request_filter");
        // resolved per request through the system resolver, so /etc/hosts and DNS changes apply at once; an
        // origin that cannot be resolved is an upstream failure, which lets the cache serve stale
        let addr = match tokio::net::lookup_host(route.origin_addr.as_str()).await {
            Ok(mut addrs) => addrs.next(),
            Err(_) => None,
        };
        let Some(addr) = addr else {
            let mut e = pingora::Error::new(pingora::ErrorType::ConnectError);
            e.as_up();
            return Err(e);
        };
        let peer = HttpPeer::new(addr, route.origin_tls, route.origin_host.clone());
        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        upstream_request: &mut pingora::http::RequestHeader,
        ctx: &mut Ctx,
    ) -> pingora::Result<()> {
        let route = ctx.account.as_ref().expect("routed");
        upstream_request.insert_header("host", route.origin_host.clone())?;
        Ok(())
    }

    fn request_cache_filter(&self, session: &mut Session, _ctx: &mut Ctx) -> pingora::Result<()> {
        if session.req_header().method == "GET" || session.req_header().method == "HEAD" {
            session
                .cache
                .enable(&*STORAGE, Some(&*EVICTION), Some(&*PREDICTOR), Some(&**LOCK), None);
        }
        Ok(())
    }

    /// Cache key = account ‖ generation ‖ host ‖ path and query (§4).
    fn cache_key_callback(&self, session: &Session, ctx: &mut Ctx) -> pingora::Result<CacheKey> {
        let route = ctx.account.as_ref().expect("routed");
        let h = session.req_header();
        let pq = h
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/");
        let primary = format!(
            "{}\0{}\0{}\0{}",
            route.account,
            route.generation,
            Self::host_of(session).unwrap_or_default(),
            pq
        );
        Ok(CacheKey::new(primary, ""))
    }

    fn response_cache_filter(
        &self,
        _session: &Session,
        resp: &ResponseHeader,
        _ctx: &mut Ctx,
    ) -> pingora::Result<RespCacheable> {
        let cc = CacheControl::from_resp_headers(resp);
        Ok(resp_cacheable(cc.as_ref(), resp.clone(), false, &META_DEFAULTS))
    }

    /// The fetch token and the cache outcome on every response.
    async fn response_filter(
        &self,
        session: &mut Session,
        upstream_response: &mut ResponseHeader,
        _ctx: &mut Ctx,
    ) -> pingora::Result<()> {
        let phase = match session.cache.phase() {
            CachePhase::Hit => "HIT",
            CachePhase::Miss => "MISS",
            CachePhase::Stale | CachePhase::StaleUpdating => "STALE",
            CachePhase::Expired => "EXPIRED",
            CachePhase::Revalidated | CachePhase::RevalidatedNoCache(_) => "REVALIDATED",
            _ => "BYPASS",
        };
        upstream_response.insert_header("x-cache", phase)?;
        upstream_response.insert_header(token::HEADER, self.token_for(session))?;
        upstream_response.insert_header("server", "dedan")?;
        Ok(())
    }

    async fn logging(&self, session: &mut Session, e: Option<&pingora::Error>, _ctx: &mut Ctx) {
        let status = session
            .response_written()
            .map(|r| r.status.as_u16())
            .unwrap_or(0);
        info!(
            method = %session.req_header().method,
            host = Self::host_of(session).unwrap_or_default(),
            path = session.req_header().uri.path(),
            status,
            cache = ?session.cache.phase(),
            error = e.map(|e| e.to_string()),
            "request"
        );
    }
}

#[allow(dead_code)]
fn _status_is_http(_: StatusCode) {}
