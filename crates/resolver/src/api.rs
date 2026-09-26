//! Management API: ACME TXT records from the control plane (the pebble-challtestsrv shape, so lego's exec
//! hook is unchanged) and read-only views of the snapshot for demos and tests.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State as AxState};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::State;
use dedan_core::types::AccountId;

pub fn router(state: Arc<State>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/nodes", get(nodes))
        .route("/map", get(map))
        .route("/assignment", get(assignment))
        .route("/cell", get(cell))
        .route("/cells", get(cells))
        .route("/heartbeat", post(heartbeat))
        .route("/set-txt", post(set_txt))
        .route("/clear-txt", post(clear_txt))
        .with_state(state)
}

#[derive(Deserialize)]
struct TxtBody {
    host: String,
    #[serde(default)]
    value: String,
}

/// lego's exec hook posts without a content type, which axum's `Json` extractor rejects with 415.
fn parse(body: &Bytes) -> Result<TxtBody, (StatusCode, String)> {
    serde_json::from_slice(body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

fn fqdn(host: &str) -> String {
    format!("{}.", host.trim_end_matches('.').to_lowercase())
}

/// `GET /cell?lat=&lon=&k=`: the H3 cell of a point and, with `k`, the cells within `k` rings of it (the
/// cells a node at that point joins); for the devnet scripts.
async fn cell(
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let num = |k: &str| {
        q.get(k)
            .and_then(|v| v.parse::<f64>().ok())
            .ok_or((StatusCode::BAD_REQUEST, format!("{k}: number expected")))
    };
    let point = dedan_core::types::LatLon::new(num("lat")?, num("lon")?);
    let cell =
        dedan_core::geo::cell(point).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let k: u32 = q.get("k").and_then(|v| v.parse().ok()).unwrap_or(0);
    let cells: Vec<String> = dedan_core::geo::disk(cell, k)
        .into_iter()
        .map(|c| c.0.to_string())
        .collect();
    Ok(Json(json!({
        "cell": cell.0.to_string(), "hex": format!("{:x}", cell.0), "lat": point.lat, "lon": point.lon, "k": k, "cells": cells,
    })))
}

/// `GET /cells`: every cell a node joined, its members, and the contract's medians for them (votes = humans).
async fn cells(AxState(s): AxState<Arc<State>>) -> Json<Value> {
    let snap = s.snap.read().await;
    let cells: Vec<Value> = snap
        .identity
        .members
        .iter()
        .map(|(cell, members)| {
            let rank = snap.map.ranks.get(cell).filter(|r| r.rings == 0);
            let centre = dedan_core::geo::centroid(*cell);
            json!({
                "cell": cell.0.to_string(),
                "name": format!("{:x}.{}", cell.0, s.cfg.parent),
                "centre": centre,
                "boundary": dedan_core::geo::boundary(*cell).iter().map(|p| [p.lat, p.lon]).collect::<Vec<_>>(),
                "members": members,
                "humans": rank.map(|r| r.humans).unwrap_or(0),
                "nodes": rank.map(|r| r.nodes.iter().map(|(n, ms)| json!({
                    "label": n, "median_ms": ms,
                    "voters": snap.map.voters.get(cell).and_then(|v| v.get(n)).cloned().unwrap_or_default(),
                })).collect::<Vec<_>>()).unwrap_or_default(),
            })
        })
        .collect();
    Json(json!({ "block": snap.block, "cells": cells }))
}

async fn health(AxState(s): AxState<Arc<State>>) -> Json<Value> {
    let snap = s.snap.read().await;
    Json(
        json!({ "ok": true, "block": snap.block, "updated_at": snap.updated_at, "nodes": snap.records.len() }),
    )
}

/// `POST /heartbeat`: a node's signed heartbeat (§5); accepted from a current `ROLE_NODE_SIGNER` key of the name.
async fn heartbeat(
    AxState(s): AxState<Arc<State>>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, String)> {
    let env: dedan_core::envelope::Envelope<dedan_core::types::Heartbeat> =
        serde_json::from_slice(&body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let now = crate::state::unix_now();
    {
        let snap = s.snap.read().await;
        let mut nonces = s.nonces.lock().await;
        env.verify(&snap.identity, now, &mut nonces)
            .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;
    }
    if env.signer != env.payload.node_id.signer() {
        return Err((StatusCode::BAD_REQUEST, "node id differs from the signer".into()));
    }
    s.heartbeats
        .write()
        .await
        .insert(env.payload.node_id.clone(), (now, env.payload));
    Ok(Json(json!({ "ok": true })))
}

async fn nodes(AxState(s): AxState<Arc<State>>) -> Json<Value> {
    let snap = s.snap.read().await;
    let beats = s.heartbeats.read().await;
    let now = crate::state::unix_now();
    let nodes: Vec<Value> = snap
        .identity
        .nodes
        .values()
        .map(|n| {
            let r = snap.records.get(&n.label);
            let hb = beats.get(&n.label);
            json!({
                "label": n.label,
                "name": n.label.name(&s.cfg.parent),
                "owner": n.owner,
                "stake": n.stake,
                "active": snap.identity.node_active(&n.label),
                "up": s.cfg.heartbeat_timeout == 0 || hb.is_some_and(|(at, h)| now.saturating_sub(*at) <= s.cfg.heartbeat_timeout && !h.drain),
                "verified": s.verified_ip(&n.label, r.and_then(|r| r.ipv4)).is_some(),
                "heartbeat": hb.map(|(at, h)| json!({ "age_s": now.saturating_sub(*at), "version": h.version, "classes": h.classes, "load_mbps": h.load_mbps })),
                "signers": snap.identity.signers(&n.label),
                "ipv4": r.and_then(|r| r.ipv4),
                "ipv6": r.and_then(|r| r.ipv6),
                "port": r.map(|r| r.port),
                "class": r.and_then(|r| r.class),
                "location": r.and_then(|r| r.location),
                "payout": r.map(|r| r.payout),
                "version": r.map(|r| r.version.clone()),
            })
        })
        .collect();
    Json(json!({ "block": snap.block, "nodes": nodes }))
}

async fn map(AxState(s): AxState<Arc<State>>) -> Json<Value> {
    let snap = s.snap.read().await;
    let ranks: Vec<Value> = snap
        .map
        .ranks
        .iter()
        .map(|(cell, r)| json!({ "cell": cell.0, "humans": r.humans, "rings": r.rings, "nodes": r.nodes }))
        .collect();
    Json(json!({
        "attestations": snap.identity.attested.len(),
        "reveals": snap.identity.reveals().len(),
        "covered": snap.map.covered.len(),
        "ranks": ranks,
        "node_location": snap.map.node_location,
        "anchor_evidence": snap.map.evidence,
        "rejected": snap.map_evidence,
    }))
}

async fn assignment(AxState(s): AxState<Arc<State>>) -> Json<Value> {
    let snap = s.snap.read().await;
    let previous = s.previous.read().await.as_ref().map(|p| p.period);
    let suspended: Vec<AccountId> = s.suspended.read().expect("suspended lock").iter().cloned().collect();
    let versions: BTreeMap<AccountId, u64> = s
        .configs
        .read()
        .expect("configs lock")
        .iter()
        .map(|(id, c)| (id.clone(), c.version))
        .collect();
    Json(json!({
        "seed": snap.assignment.seed, "sets": snap.assignment.sets, "updated_at": snap.updated_at,
        "sticky_on_period": previous, "suspended": suspended, "config_versions": versions,
    }))
}

async fn set_txt(
    AxState(s): AxState<Arc<State>>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, String)> {
    let b = parse(&body)?;
    let mut txt = s.txt.write().await;
    let entry = txt.entry(fqdn(&b.host)).or_default();
    if !entry.contains(&b.value) {
        entry.push(b.value);
    }
    Ok(Json(json!({ "ok": true })))
}

async fn clear_txt(
    AxState(s): AxState<Arc<State>>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, String)> {
    let b = parse(&body)?;
    s.txt.write().await.remove(&fqdn(&b.host));
    Ok(Json(json!({ "ok": true })))
}
