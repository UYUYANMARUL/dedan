//! The request path (ARCHITECTURE.md §4, §5): Host → account → origin, the cache under the account's cache
//! rules, the fetch token on every response, and the node's own endpoints (`/.well-known/dedan/blob/<n>`,
//! `/.well-known/dedan/challenge/<nonce>`).

use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use bytes::Bytes;
use dedan_core::token;
use dedan_core::types::CacheRules;
use pingora::cache::cache_control::{CacheControl, InterpretCacheControl};
use pingora::cache::eviction::simple_lru::Manager;
use pingora::cache::filters::{calculate_expires_header_time, resp_cacheable};
use pingora::cache::key::HashBinary;
use pingora::cache::lock::CacheLock;
use pingora::cache::predictor::Predictor;
use pingora::cache::{
    CacheKey, CacheMeta, CacheMetaDefaults, CachePhase, MemCache, NoCacheReason, RespCacheable, VarianceBuilder,
};
use pingora::http::{RequestHeader, ResponseHeader, StatusCode};
use pingora::prelude::*;
use pingora::protocols::http::error_resp;
use serde_json::json;
use tracing::{debug, info};

use crate::{unix_now, Node};

/// The cache: in memory in v1, bounded by an LRU, single-flight per key.
/// ponytail: `pingora-cache`'s `MemCache`; the disk `Storage` (file per key, atomic rename) is the next tier.
static STORAGE: LazyLock<MemCache> = LazyLock::new(MemCache::new);
static CACHE_BYTES: LazyLock<usize> = LazyLock::new(cache_bytes);
static EVICTION: LazyLock<Manager> = LazyLock::new(|| Manager::new(*CACHE_BYTES));
static PREDICTOR: LazyLock<Predictor<32>> = LazyLock::new(|| Predictor::new(128, None));
static LOCK: LazyLock<Box<CacheLock>> = LazyLock::new(|| CacheLock::new_boxed(Duration::from_secs(5)));
/// Which statuses are cacheable when the origin says nothing (fresh for the account's TTL, see
/// `response_cache_filter`); stale-while-revalidate 30 s, stale-if-error 24 h (§5: serve stale on origin
/// failure for up to 24 h), unless the origin's Cache-Control says otherwise.
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

/// The account's TTL for responses the origin gives no freshness; the node's default when the config has none.
fn ttl_of(rules: &CacheRules) -> u64 {
    match rules.default_ttl_s {
        0 => default_ttl(),
        s => s.into(),
    }
}

/// Cache key = account ‖ generation ‖ host ‖ path, and the query unless the account's rules drop it (§4, §9).
pub fn cache_primary(route: &crate::Route, host: &str, uri: &http::Uri) -> String {
    let path = match (route.rules.keep_query_string, uri.path_and_query()) {
        (true, Some(pq)) => pq.as_str(),
        _ => uri.path(),
    };
    format!("{}\0{}\0{}\0{}", route.account, route.generation, host, path)
}

/// The request headers a response varies on (`Vary`), lowercased; `*` means it cannot be cached.
fn vary_names(headers: &http::HeaderMap) -> impl Iterator<Item = String> + '_ {
    headers
        .get_all("vary")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|n| n.trim().to_ascii_lowercase())
        .filter(|n| !n.is_empty())
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
        // Pingora 0.9 replays a request as HTTP/1 text for the background refresh of stale-while-revalidate
        // (`pseudo_raw_h1_request_header`). An HTTP/2 request comes out with `HTTP/2` in the request line, which its
        // own HTTP/1 parser refuses, and without a Host header (HTTP/2 names the host in `:authority`), so the
        // refresh never ran and the object stayed stale. Both are informational for an HTTP/2 downstream, and the
        // upstream request is HTTP/1.1 anyway.
        if session.req_header().version == http::Version::HTTP_2 {
            session.req_header_mut().set_version(http::Version::HTTP_11);
        }
        if session.req_header().headers.get("host").is_none() {
            session.req_header_mut().insert_header("host", host.as_str())?;
        }
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

    fn request_cache_filter(&self, session: &mut Session, ctx: &mut Ctx) -> pingora::Result<()> {
        if session.req_header().method == "GET" || session.req_header().method == "HEAD" {
            session
                .cache
                .enable(&*STORAGE, Some(&*EVICTION), Some(&*PREDICTOR), Some(&**LOCK), None);
            // an object over the account's size (or the whole cache) streams through without being held in memory
            let rules = &ctx.account.as_ref().expect("routed").rules;
            let max = usize::try_from(rules.max_object_bytes).unwrap_or(usize::MAX);
            session
                .cache
                .set_max_file_size_bytes(if max == 0 { *CACHE_BYTES } else { max.min(*CACHE_BYTES) });
        }
        Ok(())
    }

    fn cache_key_callback(&self, session: &Session, ctx: &mut Ctx) -> pingora::Result<CacheKey> {
        let route = ctx.account.as_ref().expect("routed");
        let host = Self::host_of(session).unwrap_or_default();
        Ok(CacheKey::new(cache_primary(route, &host, &session.req_header().uri), ""))
    }

    /// Cacheable per the origin's headers; fresh for the account's TTL when the origin says nothing; never when
    /// it varies on everything (`Vary: *`).
    fn response_cache_filter(
        &self,
        _session: &Session,
        resp: &ResponseHeader,
        ctx: &mut Ctx,
    ) -> pingora::Result<RespCacheable> {
        if vary_names(&resp.headers).any(|n| n == "*") {
            return Ok(RespCacheable::Uncacheable(NoCacheReason::OriginNotCache));
        }
        let cc = CacheControl::from_resp_headers(resp);
        let mut cacheable = resp_cacheable(cc.as_ref(), resp.clone(), false, &META_DEFAULTS);
        let origin_ttl = cc.as_ref().and_then(|c| c.fresh_duration()).is_some()
            || calculate_expires_header_time(resp).is_some();
        if let (RespCacheable::Cacheable(meta), Some(route), false) = (&mut cacheable, ctx.account.as_ref(), origin_ttl) {
            let (swr, sie) = (meta.stale_while_revalidate_sec(), meta.stale_if_error_sec());
            meta.update_freshness(SystemTime::now() + Duration::from_secs(ttl_of(&route.rules)), swr, sie);
        }
        Ok(cacheable)
    }

    /// `Vary`: one cached variant per value of the request headers the origin names.
    fn cache_vary_filter(&self, meta: &CacheMeta, _ctx: &mut Ctx, req: &RequestHeader) -> Option<HashBinary> {
        let mut variance = VarianceBuilder::new();
        for name in vary_names(meta.headers()) {
            let value = req
                .headers
                .get_all(name.as_str())
                .iter()
                .map(|v| v.as_bytes())
                .collect::<Vec<_>>()
                .join(&b","[..]);
            variance.add_owned_name_value(name, value);
        }
        variance.finalize()
    }

    /// Stale while revalidating (`error` is `None`; Pingora has already checked the object is inside its
    /// stale-while-revalidate window) and stale when the origin fails (stale-if-error), not otherwise.
    fn should_serve_stale(&self, _session: &mut Session, _ctx: &mut Ctx, error: Option<&pingora::Error>) -> bool {
        error.is_none_or(|e| e.esource() == &pingora::ErrorSource::Upstream)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_is_in_the_key_only_when_the_account_keeps_it() {
        let mut route = crate::Route {
            account: "acme".into(),
            origin_addr: "origin:80".into(),
            origin_tls: false,
            origin_host: "origin".into(),
            rules: CacheRules {
                default_ttl_s: 60,
                keep_query_string: true,
                max_object_bytes: 1 << 20,
            },
            generation: 2,
        };
        let uri: http::Uri = "/a.png?v=1".parse().unwrap();
        assert_eq!(cache_primary(&route, "www.images.com", &uri), "acme\u{0}2\u{0}www.images.com\u{0}/a.png?v=1");
        route.rules.keep_query_string = false;
        assert_eq!(cache_primary(&route, "www.images.com", &uri), "acme\u{0}2\u{0}www.images.com\u{0}/a.png");
        assert_eq!(ttl_of(&route.rules), 60);
    }
}
