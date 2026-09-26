//! Measurements: a TCP round trip, and an HTTP fetch that looks like a user's (the customer hostname as
//! `Host`, the body decoded and hashed), returning the node's fetch token and the timings the scoring reads.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use dedan_core::token;
use dedan_core::types::FailureClass;
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub struct Fetch {
    pub status: u16,
    pub ttfb_ms: u32,
    pub token: Option<String>,
    pub bytes: u64,
    pub sha256: String,
    pub kbps: u64,
}

/// The shortest of `tries` TCP connects to `addr`, in ms, with the local address the peer saw.
pub async fn tcp_rtt(addr: SocketAddr, tries: u32) -> Option<(u32, IpAddr)> {
    let mut best: Option<(u32, IpAddr)> = None;
    for _ in 0..tries {
        let t0 = Instant::now();
        let Ok(Ok(s)) = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await else {
            continue;
        };
        let ms = t0.elapsed().as_millis() as u32;
        let local = s.local_addr().map(|a| a.ip()).unwrap_or(addr.ip());
        if best.is_none_or(|(b, _)| ms < b) {
            best = Some((ms, local));
        }
    }
    best
}

/// A client that connects `host` to `addr` whatever DNS says: how a user whose resolver answered that IP
/// reaches the node, and how the schedule aims at one slot member.
pub fn client_for(host: &str, addr: SocketAddr) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .resolve(host, addr)
        .build()
        .expect("reqwest client")
}

/// A client that resolves names through the system resolver, as a landing probe must.
pub fn landing_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("reqwest client")
}

fn classify(e: &reqwest::Error) -> FailureClass {
    if e.is_timeout() {
        FailureClass::Timeout
    } else if format!("{e:?}").to_lowercase().contains("dns") {
        FailureClass::Dns
    } else {
        FailureClass::Connect
    }
}

/// One GET of `url`: status, time to first byte, the node's token, the decoded body's size and hash, and
/// the single-stream throughput in kbit/s. A 5xx is a failure of class `Http5xx`; the token is still read.
pub async fn fetch(client: &reqwest::Client, url: &str) -> Result<Fetch, FailureClass> {
    let t0 = Instant::now();
    let resp = client.get(url).send().await.map_err(|e| classify(&e))?;
    let ttfb_ms = t0.elapsed().as_millis() as u32;
    let status = resp.status().as_u16();
    let token = resp
        .headers()
        .get(token::HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = resp.bytes().await.map_err(|e| classify(&e))?;
    let total = t0.elapsed();
    let bytes = body.len() as u64;
    let secs = total.as_secs_f64().max(0.001);
    Ok(Fetch {
        status,
        ttfb_ms,
        token,
        bytes,
        sha256: hex::encode(Sha256::digest(&body)),
        kbps: (bytes as f64 * 8.0 / secs / 1000.0) as u64,
    })
}
