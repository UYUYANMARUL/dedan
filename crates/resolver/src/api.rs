//! Management API: ACME TXT records from the control plane (the pebble-challtestsrv shape, so lego's exec
//! hook is unchanged) and read-only views of the snapshot for demos and tests.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State as AxState};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::State;

pub fn router(state: Arc<State>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/nodes", get(nodes))
        .route("/map", get(map))
        .route("/assignment", get(assignment))
        .route("/cell", get(cell))
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

/// `GET /cell?lat=&lon=`: the H3 cell the map keys on, for scripts that add cities to the devnet.
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
    Ok(Json(
        json!({ "cell": cell.0.to_string(), "lat": point.lat, "lon": point.lon }),
    ))
}

async fn health(AxState(s): AxState<Arc<State>>) -> Json<Value> {
    let snap = s.snap.read().await;
    Json(
        json!({ "ok": true, "block": snap.block, "updated_at": snap.updated_at, "nodes": snap.records.len() }),
    )
}

async fn nodes(AxState(s): AxState<Arc<State>>) -> Json<Value> {
    let snap = s.snap.read().await;
    let nodes: Vec<Value> = snap
        .identity
        .nodes
        .values()
        .map(|n| {
            let r = snap.records.get(&n.label);
            json!({
                "label": n.label,
                "name": n.label.name(&s.cfg.parent),
                "owner": n.owner,
                "stake": n.stake,
                "active": snap.identity.node_active(&n.label),
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
    Json(
        json!({ "seed": snap.assignment.seed, "sets": snap.assignment.sets, "updated_at": snap.updated_at }),
    )
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
