//! `dedan-resolver`: the authoritative nameserver of `dedan-cdn.<tld>` (ARCHITECTURE.md §6).
//!
//! Every answer is computed from public data: node names and records from ENS through the Universal
//! Resolver, roles and attestations from the Dedan registry's events, the latency map and the slot
//! assignment from `dedan-core`. Nothing on the query path touches the chain; a sync loop refreshes a
//! snapshot every few seconds. The management API serves the ACME TXT records the control plane publishes
//! and exposes the snapshot for demos and tests.

mod api;
mod dns;
mod state;

use std::sync::Arc;
use std::time::Duration;

use hickory_server::Server;
use tokio::net::{TcpListener, UdpSocket};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cfg = state::Config::from_env()?;
    info!(zone = %cfg.zone, parent = %cfg.parent, rpc = %cfg.rpc_url, "starting");
    let state = Arc::new(state::State::new(cfg.clone()).await?);
    if let Err(e) = state.refresh().await {
        warn!(error = %e, "first refresh failed; serving from an empty snapshot until the chain answers");
    }

    // sync loop: registry events, ENS records, map, assignment
    let sync = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(sync.cfg.sync_secs));
        loop {
            tick.tick().await;
            if let Err(e) = sync.refresh().await {
                warn!(error = %e, "refresh failed");
            }
        }
    });

    // management API
    let api_state = state.clone();
    let http_addr = cfg.http_addr;
    tokio::spawn(async move {
        let listener = TcpListener::bind(http_addr).await.expect("bind http");
        info!(%http_addr, "management api listening");
        if let Err(e) = axum::serve(listener, api::router(api_state)).await {
            error!(error = %e, "http server stopped");
        }
    });

    // DNS on UDP and TCP
    let mut server = Server::new(dns::Handler::new(state.clone()));
    server.register_socket(UdpSocket::bind(cfg.dns_addr).await?);
    server.register_listener(
        TcpListener::bind(cfg.dns_addr).await?,
        Duration::from_secs(5),
        65_535,
    );
    info!(dns_addr = %cfg.dns_addr, "nameserver listening");
    server.block_until_done().await?;
    Ok(())
}
