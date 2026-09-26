//! The latency map (ARCHITECTURE.md §6, §7): where the humans measure each node to be.
//!
//! Attestations are `Attested` events of the cells contract, one per round trip a verified human measured
//! to a member of its H3 cell. The contract keeps the map itself: per human, cell and node the newest
//! sample is the vote, votes land in the day's histogram, and a cell's record for a node is the median over
//! today's and yesterday's votes. [`Map::from_votes`] mirrors exactly those rules from the events, so every
//! resolver, the aggregator, the countersigner and `recompute` hold the contract's map without asking it.
//! The [`Validator`] applies the checks the contract cannot (the fetch token, the network's location) for
//! auditor pay and governance evidence; [`Map::build`] over its rows is the same map computed from raw
//! samples, used by tests and the anchor rules.

use crate::geo;
use crate::token::{self, Token};
use crate::types::*;
use alloy::primitives::{Address, B256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapParams {
    /// Distinct humans a cell needs before it counts (1 on the devnet).
    pub k_min: usize,
    /// How far an uncovered cell looks for measurements.
    pub rings: u32,
    pub window_s: u64,
    /// One cell per human per epoch.
    pub epoch_s: u64,
    /// The client prefix inside the token must geolocate within this distance of the attested cell.
    pub geo_radius_km: f64,
    pub pay_cap: usize,
    pub anchor_clamp_ms: u32,
    /// The contract's histogram: `buckets` buckets of `bucket_ms` each, the last one open-ended.
    pub bucket_ms: u32,
    pub buckets: usize,
}

impl MapParams {
    pub fn v1() -> Self {
        Self {
            k_min: 3,
            rings: 3,
            window_s: 48 * 3600,
            epoch_s: 24 * 3600,
            geo_radius_km: 250.0,
            pay_cap: 50,
            anchor_clamp_ms: 50,
            bucket_ms: 8,
            buckets: 32,
        }
    }

    pub fn devnet() -> Self {
        Self {
            k_min: 1,
            ..Self::v1()
        }
    }

    pub fn epoch_of(&self, ts: u64) -> u64 {
        ts / self.epoch_s
    }
}

/// The `Attested` event as the chain carries it, before validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedEvent {
    pub attester: Address,
    pub cell: Cell,
    pub label_hash: B256,
    pub rtt_ms: u32,
    pub hit_ttfb_ms: u32,
    pub token: String,
    pub prefix: String,
    /// Block time.
    pub ts: u64,
    pub block: u64,
}

/// An attestation that passed every rule; what the map is built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidAttestation {
    pub auditor: AuditorId,
    pub cell: Cell,
    pub node: NodeId,
    pub rtt_ms: u32,
    pub hit_ttfb_ms: Option<u32>,
    pub ts: u64,
    pub anchor: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    UnknownNode,
    WrongEpoch,
    BadToken,
    UnknownLocation,
    OutOfArea { km: f64 },
}

/// The nodes that joined each cell.
pub type Membership = BTreeMap<Cell, BTreeSet<NodeId>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub auditor: AuditorId,
    pub node: Option<NodeId>,
    pub cell: Cell,
    pub ts: u64,
    pub reason: Rejection,
}

/// Verifies a fetch token for a node against the key that node revealed.
pub trait TokenCheck {
    fn check(&self, token: &str, node: &NodeId, prefix: &str) -> bool;
}

/// Locates a client prefix (`10.77.0.0/24`): GeoIP in production, a table on the devnet.
pub trait GeoLocate {
    fn locate(&self, prefix: &str) -> Option<LatLon>;
}

impl GeoLocate for HashMap<String, LatLon> {
    fn locate(&self, prefix: &str) -> Option<LatLon> {
        self.get(prefix).copied()
    }
}

/// Key reveals by (node, hour) from `KeyRevealed` events: the [`TokenCheck`] of anyone reading the chain.
#[derive(Debug, Clone, Default)]
pub struct RevealSet {
    keys: HashMap<(NodeId, u64), B256>,
}

impl RevealSet {
    pub fn add(&mut self, reveal: &KeyReveal) {
        self.keys
            .insert((reveal.node_id.clone(), reveal.hour), reveal.k_hour);
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

impl TokenCheck for RevealSet {
    fn check(&self, token: &str, node: &NodeId, prefix: &str) -> bool {
        let Ok(t) = token.parse::<Token>() else {
            return false;
        };
        let hour = t.minute / 60;
        let Some(k) = self.keys.get(&(node.clone(), hour)) else {
            return false;
        };
        token::verify(
            &t,
            &KeyReveal {
                node_id: node.clone(),
                hour,
                k_hour: *k,
            },
            prefix,
        )
    }
}

/// The checks the contract cannot make (§7): the fetch token against the key the node revealed, and the
/// network inside the token within `geo_radius_km` of the attested cell. Everything else (membership, the
/// batch cadence, one vote per human per node per cell) the contract enforced before the event existed.
pub struct Validator<'a> {
    params: MapParams,
    tokens: &'a dyn TokenCheck,
    geo: &'a dyn GeoLocate,
}

impl<'a> Validator<'a> {
    pub fn new(params: MapParams, tokens: &'a dyn TokenCheck, geo: &'a dyn GeoLocate) -> Self {
        Self {
            params,
            tokens,
            geo,
        }
    }

    /// One attestation. `anchor` marks rows produced during assigned probes.
    pub fn validate(
        &mut self,
        auditor: AuditorId,
        a: &Attestation,
    ) -> Result<ValidAttestation, Evidence> {
        let reject = |reason: Rejection| Evidence {
            auditor,
            node: Some(a.node_id.clone()),
            cell: a.cell,
            ts: a.ts,
            reason,
        };
        if !self.tokens.check(&a.token, &a.node_id, &a.prefix) {
            return Err(reject(Rejection::BadToken));
        }
        let (Some(here), Some(centre)) = (self.geo.locate(&a.prefix), geo::centroid(a.cell)) else {
            return Err(reject(Rejection::UnknownLocation));
        };
        let km = geo::distance_km(here, centre);
        if km > self.params.geo_radius_km {
            return Err(reject(Rejection::OutOfArea { km }));
        }
        Ok(ValidAttestation {
            auditor,
            cell: a.cell,
            node: a.node_id.clone(),
            rtt_ms: a.rtt_ms,
            hit_ttfb_ms: a.hit_ttfb_ms,
            ts: a.ts,
            anchor: a.anchor,
        })
    }

    /// A batch the app assembled; every row must fall in the batch's epoch.
    pub fn validate_batch(
        &mut self,
        batch: &AttestationBatch,
    ) -> (Vec<ValidAttestation>, Vec<Evidence>) {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        let mut rows: Vec<&Attestation> = batch.attestations.iter().collect();
        rows.sort_by_key(|a| a.ts);
        for a in rows {
            if self.params.epoch_of(a.ts) != batch.epoch {
                bad.push(Evidence {
                    auditor: batch.auditor_id,
                    node: Some(a.node_id.clone()),
                    cell: a.cell,
                    ts: a.ts,
                    reason: Rejection::WrongEpoch,
                });
                continue;
            }
            match self.validate(batch.auditor_id, a) {
                Ok(v) => ok.push(v),
                Err(e) => bad.push(e),
            }
        }
        (ok, bad)
    }

    /// `Attested` events in chain order; `labels` maps the registry's label hashes to node ids.
    pub fn validate_events(
        &mut self,
        events: &[AttestedEvent],
        labels: &HashMap<B256, NodeId>,
    ) -> (Vec<ValidAttestation>, Vec<Evidence>) {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        for e in events {
            let auditor = AuditorId(e.attester);
            let Some(node) = labels.get(&e.label_hash) else {
                bad.push(Evidence {
                    auditor,
                    node: None,
                    cell: e.cell,
                    ts: e.ts,
                    reason: Rejection::UnknownNode,
                });
                continue;
            };
            let a = Attestation {
                cell: e.cell,
                node_id: node.clone(),
                rtt_ms: e.rtt_ms,
                hit_ttfb_ms: (e.hit_ttfb_ms > 0).then_some(e.hit_ttfb_ms),
                token: e.token.clone(),
                prefix: e.prefix.clone(),
                ts: e.ts,
                anchor: false,
            };
            match self.validate(auditor, &a) {
                Ok(v) => ok.push(v),
                Err(err) => bad.push(err),
            }
        }
        (ok, bad)
    }
}

/// Per human: (crowd samples, anchor samples) in ms.
type Samples = BTreeMap<AuditorId, (Vec<u32>, Vec<u32>)>;

/// Nodes in a cell's order, nearest first. `rings` is how far the cell had to look to reach `k_min` humans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellRank {
    pub humans: usize,
    pub rings: u32,
    /// `(node, median round trip in ms)`, ascending; ties by node id.
    pub nodes: Vec<(NodeId, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorDisagreement {
    pub cell: Cell,
    pub node: NodeId,
    pub crowd_ms: u32,
    pub anchor_ms: u32,
    pub auditors: Vec<AuditorId>,
}

/// One human's current vote for a node in a cell: its newest sample inside the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Voter {
    pub attester: Address,
    pub rtt_ms: u32,
    pub ts: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Map {
    /// Every cell within `rings` of a measured cell that reached `k_min` humans, directly or by ring search.
    pub ranks: BTreeMap<Cell, CellRank>,
    /// The votes behind each cell's medians (`from_votes` only): who, how many ms, when; by rtt.
    pub voters: BTreeMap<Cell, BTreeMap<NodeId, Vec<Voter>>>,
    /// Cells covered without ring search: what the assignment optimises over.
    pub covered: BTreeSet<Cell>,
    pub nodes_seen: BTreeSet<NodeId>,
    pub node_location: BTreeMap<NodeId, LatLon>,
    pub evidence: Vec<AnchorDisagreement>,
    #[serde(skip)]
    rows: BTreeMap<Cell, Vec<ValidAttestation>>,
}

impl Map {
    pub fn build(records: &[ValidAttestation], now: u64, params: &MapParams) -> Map {
        let mut rows: BTreeMap<Cell, Vec<ValidAttestation>> = BTreeMap::new();
        for r in records {
            if now.saturating_sub(r.ts) <= params.window_s {
                rows.entry(r.cell).or_default().push(r.clone());
            }
        }
        let mut map = Map {
            rows,
            ..Default::default()
        };
        map.nodes_seen = map
            .rows
            .values()
            .flatten()
            .map(|r| r.node.clone())
            .collect();
        let measured: Vec<Cell> = map.rows.keys().copied().collect();
        let mut evidence = Vec::new();
        // ponytail: every measured cell expands to 37 neighbours, each evaluated over up to 4 disks; fine
        // until the map has tens of thousands of measured cells, then cache disks per cell
        for c in &measured {
            for d in geo::disk(*c, params.rings) {
                if map.ranks.contains_key(&d) {
                    continue;
                }
                if let Some(rank) = map.rank_for(d, params, &mut evidence) {
                    if rank.rings == 0 {
                        map.covered.insert(d);
                    }
                    map.ranks.insert(d, rank);
                }
            }
        }
        map.evidence = evidence;
        map.node_location = map.estimate_locations();
        map
    }

    /// The contract's map, mirrored from its events (`DedanCells.attest`): per human, cell and node the newest
    /// sample is the vote and withdraws the previous one when that sits in today's or yesterday's histogram;
    /// a cell's record for a node is the lower-middle median over both days, as the bucket midpoint. Cells
    /// without `k_min` votes borrow the merged histograms of their neighbours, ring by ring. `now` is the
    /// chain's clock (the head block's time), so the window is the contract's.
    pub fn from_votes(
        events: &[AttestedEvent],
        _members: &Membership,
        labels: &HashMap<B256, NodeId>,
        now: u64,
        params: &MapParams,
    ) -> Map {
        let today = now / params.epoch_s;
        let nb = params.buckets;
        // per human, cell and node: (epoch, bucket, rtt, ts) of its current vote
        let mut votes: HashMap<(Address, Cell, NodeId), (u64, usize, u32, u64)> = HashMap::new();
        let mut hist: HashMap<(Cell, NodeId, u64), Vec<u32>> = HashMap::new();
        for e in events {
            let Some(node) = labels.get(&e.label_hash) else {
                continue;
            };
            let epoch = e.ts / params.epoch_s;
            let bucket = ((e.rtt_ms / params.bucket_ms) as usize).min(nb - 1);
            let key = (e.attester, e.cell, node.clone());
            if let Some((pe, pb, _, _)) = votes.get(&key) {
                if pe + 1 >= epoch {
                    if let Some(h) = hist.get_mut(&(e.cell, node.clone(), *pe)) {
                        h[*pb] = h[*pb].saturating_sub(1);
                    }
                }
            }
            hist.entry((e.cell, node.clone(), epoch))
                .or_insert_with(|| vec![0; nb])[bucket] += 1;
            votes.insert(key, (epoch, bucket, e.rtt_ms, e.ts));
        }
        let mut voters: BTreeMap<Cell, BTreeMap<NodeId, Vec<Voter>>> = BTreeMap::new();
        for ((attester, cell, node), (epoch, _, rtt_ms, ts)) in &votes {
            if *epoch + 1 < today || *epoch > today {
                continue;
            }
            voters
                .entry(*cell)
                .or_default()
                .entry(node.clone())
                .or_default()
                .push(Voter {
                    attester: *attester,
                    rtt_ms: *rtt_ms,
                    ts: *ts,
                });
        }
        for nodes in voters.values_mut() {
            for v in nodes.values_mut() {
                v.sort_by_key(|x| (x.rtt_ms, x.attester));
            }
        }
        // the window: today and yesterday, merged per cell and node; nodes without a vote left drop out
        let mut window: BTreeMap<Cell, BTreeMap<NodeId, Vec<u32>>> = BTreeMap::new();
        for ((cell, node, epoch), h) in &hist {
            if *epoch + 1 < today || *epoch > today {
                continue;
            }
            let w = window
                .entry(*cell)
                .or_default()
                .entry(node.clone())
                .or_insert_with(|| vec![0; nb]);
            for (i, c) in h.iter().enumerate() {
                w[i] += c;
            }
        }
        for nodes in window.values_mut() {
            nodes.retain(|_, h| h.iter().sum::<u32>() > 0);
        }
        window.retain(|_, nodes| !nodes.is_empty());
        let mut map = Map {
            voters,
            ..Map::default()
        };
        map.nodes_seen = window
            .values()
            .flat_map(|m| m.keys().cloned())
            .collect();
        let measured: Vec<Cell> = window.keys().copied().collect();
        for c in &measured {
            for d in geo::disk(*c, params.rings) {
                if map.ranks.contains_key(&d) {
                    continue;
                }
                if let Some(rank) = Self::rank_votes(d, &window, params) {
                    if rank.rings == 0 {
                        map.covered.insert(d);
                    }
                    map.ranks.insert(d, rank);
                }
            }
        }
        map.node_location = map.estimate_locations();
        map
    }

    /// The first ring around `cell` whose merged histograms reach `k_min` votes for some node.
    fn rank_votes(
        cell: Cell,
        window: &BTreeMap<Cell, BTreeMap<NodeId, Vec<u32>>>,
        params: &MapParams,
    ) -> Option<CellRank> {
        for r in 0..=params.rings {
            let mut merged: BTreeMap<NodeId, Vec<u32>> = BTreeMap::new();
            for c in geo::disk(cell, r) {
                let Some(nodes) = window.get(&c) else { continue };
                for (n, h) in nodes {
                    let m = merged
                        .entry(n.clone())
                        .or_insert_with(|| vec![0; h.len()]);
                    for (i, v) in h.iter().enumerate() {
                        m[i] += v;
                    }
                }
            }
            let humans = merged
                .values()
                .map(|h| h.iter().sum::<u32>())
                .max()
                .unwrap_or(0) as usize;
            if humans < params.k_min {
                continue;
            }
            let mut nodes: Vec<(NodeId, u32)> = merged
                .iter()
                .map(|(n, h)| (n.clone(), histogram_median(h, params.bucket_ms)))
                .collect();
            nodes.sort_by(|(na, va), (nb, vb)| (va, na).cmp(&(vb, nb)));
            return Some(CellRank {
                humans,
                rings: r,
                nodes,
            });
        }
        None
    }

    fn rank_for(
        &self,
        cell: Cell,
        params: &MapParams,
        evidence: &mut Vec<AnchorDisagreement>,
    ) -> Option<CellRank> {
        for r in 0..=params.rings {
            let cells = geo::disk(cell, r);
            let rows: Vec<&ValidAttestation> = cells
                .iter()
                .filter_map(|c| self.rows.get(c))
                .flatten()
                .collect();
            let humans: BTreeSet<AuditorId> = rows.iter().map(|x| x.auditor).collect();
            if humans.len() < params.k_min {
                continue;
            }
            let mut per_node: BTreeMap<NodeId, Samples> = BTreeMap::new();
            for x in &rows {
                let e = per_node
                    .entry(x.node.clone())
                    .or_default()
                    .entry(x.auditor)
                    .or_default();
                if x.anchor {
                    e.1.push(x.rtt_ms);
                } else {
                    e.0.push(x.rtt_ms);
                }
            }
            let mut nodes = Vec::new();
            for (node, by_human) in per_node {
                let crowd: Vec<u32> = by_human
                    .values()
                    .filter(|(c, _)| !c.is_empty())
                    .map(|(c, _)| median(c))
                    .collect();
                let anchors: Vec<u32> = by_human
                    .values()
                    .filter(|(_, a)| !a.is_empty())
                    .map(|(_, a)| median(a))
                    .collect();
                let all: Vec<u32> = by_human
                    .values()
                    .map(|(c, a)| median(&[c.as_slice(), a.as_slice()].concat()))
                    .collect();
                let mut value = median(&all);
                if !crowd.is_empty() && !anchors.is_empty() {
                    let (anchor_ms, crowd_ms) = (median(&anchors), median(&crowd));
                    if anchor_ms.abs_diff(crowd_ms) > params.anchor_clamp_ms {
                        if r == 0 {
                            evidence.push(AnchorDisagreement {
                                cell,
                                node: node.clone(),
                                crowd_ms,
                                anchor_ms,
                                auditors: by_human
                                    .iter()
                                    .filter(|(_, (c, _))| !c.is_empty())
                                    .map(|(a, _)| *a)
                                    .collect(),
                            });
                        }
                        value = anchor_ms;
                    }
                }
                nodes.push((node, value));
            }
            nodes.sort_by(|(na, va), (nb, vb)| (va, na).cmp(&(vb, nb)));
            return Some(CellRank {
                humans: humans.len(),
                rings: r,
                nodes,
            });
        }
        None
    }

    /// Node order for a cell, or `None` when nothing within `rings` is covered (cold start at the resolver).
    pub fn rank(&self, cell: Cell) -> Option<&CellRank> {
        self.ranks.get(&cell)
    }

    pub fn rtt(&self, cell: Cell, node: &NodeId) -> Option<u32> {
        self.ranks
            .get(&cell)?
            .nodes
            .iter()
            .find(|(n, _)| n == node)
            .map(|(_, v)| *v)
    }

    pub fn location(&self, node: &NodeId) -> Option<LatLon> {
        self.node_location.get(node).copied()
    }

    /// Where a node is, from the map alone: the centroid of the directly covered cells where it ranks first,
    /// or of every cell it appears in when it is first nowhere.
    fn estimate_locations(&self) -> BTreeMap<NodeId, LatLon> {
        let mut best: BTreeMap<NodeId, Vec<Cell>> = BTreeMap::new();
        let mut any: BTreeMap<NodeId, Vec<Cell>> = BTreeMap::new();
        for (cell, rank) in &self.ranks {
            if rank.rings != 0 {
                continue;
            }
            if let Some((first, _)) = rank.nodes.first() {
                best.entry(first.clone()).or_default().push(*cell);
            }
            for (n, _) in &rank.nodes {
                any.entry(n.clone()).or_default().push(*cell);
            }
        }
        any.iter()
            .map(|(n, cells)| {
                let cells = best.get(n).unwrap_or(cells);
                let pts: Vec<LatLon> = cells.iter().filter_map(|c| geo::centroid(*c)).collect();
                let k = pts.len().max(1) as f64;
                let lat = pts.iter().map(|p| p.lat).sum::<f64>() / k;
                let lon = pts.iter().map(|p| p.lon).sum::<f64>() / k;
                (n.clone(), LatLon::new(lat, lon))
            })
            .collect()
    }
}

/// The lower-middle median of a histogram as the bucket midpoint, what `DedanCells.median` answers; 0 when empty.
pub fn histogram_median(h: &[u32], bucket_ms: u32) -> u32 {
    let total: u32 = h.iter().sum();
    if total == 0 {
        return 0;
    }
    let half = (total - 1) / 2;
    let mut acc = 0;
    for (b, c) in h.iter().enumerate() {
        acc += c;
        if acc > half {
            return b as u32 * bucket_ms + bucket_ms / 2;
        }
    }
    0
}

/// Lower-middle median of an unsorted slice; 0 for an empty one.
pub fn median(values: &[u32]) -> u32 {
    if values.is_empty() {
        return 0;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        ((v[n / 2 - 1] as u64 + v[n / 2] as u64) / 2) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::fixtures::*;

    struct AcceptOk;
    impl TokenCheck for AcceptOk {
        fn check(&self, token: &str, _: &NodeId, _: &str) -> bool {
            token == "ok"
        }
    }

    const IST_PREFIX: &str = "10.77.0.0/24";
    const EPOCH: u64 = 20_700; // days since the epoch; ts inside it below
    const TS: u64 = EPOCH * 86_400 + 3_600;

    fn geoip() -> HashMap<String, LatLon> {
        HashMap::from([(IST_PREFIX.to_string(), ISTANBUL)])
    }

    fn auditor(n: u8) -> AuditorId {
        AuditorId(Address::from([n; 20]))
    }

    fn node(s: &str) -> NodeId {
        NodeId::new(s)
    }

    fn att(cell: Cell, node_label: &str, rtt: u32, ts: u64, token: &str) -> Attestation {
        Attestation {
            cell,
            node_id: node(node_label),
            rtt_ms: rtt,
            hit_ttfb_ms: None,
            token: token.into(),
            prefix: IST_PREFIX.into(),
            ts,
            anchor: false,
        }
    }

    fn batch(a: u8, attestations: Vec<Attestation>) -> AttestationBatch {
        AttestationBatch {
            auditor_id: auditor(a),
            cell: c(ISTANBUL),
            epoch: EPOCH,
            attestations,
        }
    }

    fn ev(a: u8, cell: Cell, label: &str, rtt: u32, ts: u64) -> AttestedEvent {
        AttestedEvent {
            attester: Address::from([a; 20]),
            cell,
            label_hash: node(label).label_hash(),
            rtt_ms: rtt,
            hit_ttfb_ms: 0,
            token: "ok".into(),
            prefix: IST_PREFIX.into(),
            ts,
            block: 1,
        }
    }

    fn row(a: u8, cell: Cell, node_label: &str, rtt: u32, anchor: bool) -> ValidAttestation {
        ValidAttestation {
            auditor: auditor(a),
            cell,
            node: node(node_label),
            rtt_ms: rtt,
            hit_ttfb_ms: None,
            ts: TS,
            anchor,
        }
    }

    #[test]
    fn validator_applies_every_rule() {
        let geo = geoip();
        let mut v = Validator::new(MapParams::v1(), &AcceptOk, &geo);
        let ist = c(ISTANBUL);
        let near = geo::disk(ist, 1)[1];
        let mut tokyo_from_istanbul = att(c(TOKYO), "tok", 5, TS + 4, "ok");
        tokyo_from_istanbul.prefix = IST_PREFIX.into();
        let mut unknown = att(ist, "fra", 10, TS + 7, "ok");
        unknown.prefix = "10.9.9.0/24".into();
        let b = batch(
            1,
            vec![
                att(ist, "fra", 10, TS, "ok"),
                att(ist, "fra", 11, TS + 1, "ok"), // again: the contract keeps the newest vote, the validator both
                att(near, "tok", 20, TS + 5, "ok"), // the neighbouring cell, 300 km from this network: out of area
                tokyo_from_istanbul, // Tokyo cell from an Istanbul network: out of area
                att(ist, "sel", 30, TS + 6, "bad"), // token does not verify
                unknown,             // a network nobody can place
                att(ist, "sel", 30, TS + 200_000, "ok"), // outside the batch's epoch
            ],
        );
        let (ok, bad) = v.validate_batch(&b);
        assert_eq!(ok.len(), 2);
        let reasons: Vec<&Rejection> = bad.iter().map(|e| &e.reason).collect();
        assert!(matches!(reasons[0], Rejection::OutOfArea { km } if *km > 8_000.0));
        assert!(matches!(reasons[1], Rejection::OutOfArea { km } if *km > 250.0 && *km < 1_000.0));
        assert!(matches!(reasons[2], Rejection::BadToken));
        assert!(matches!(reasons[3], Rejection::UnknownLocation));
        assert!(matches!(reasons[4], Rejection::WrongEpoch));
    }

    #[test]
    fn events_need_a_known_label_and_real_tokens() {
        let geo = geoip();
        let k = B256::from([9u8; 32]);
        let mut reveals = RevealSet::default();
        reveals.add(&KeyReveal {
            node_id: node("fra"),
            hour: token::hour_of(TS),
            k_hour: k,
        });
        let good = token::mint(&node("fra"), token::minute_of(TS), &k.0, IST_PREFIX).to_string();
        let labels = HashMap::from([(node("fra").label_hash(), node("fra"))]);
        let ev = |label_hash: B256, tok: &str| AttestedEvent {
            attester: Address::from([1u8; 20]),
            cell: c(ISTANBUL),
            label_hash,
            rtt_ms: 12,
            hit_ttfb_ms: 0,
            token: tok.into(),
            prefix: IST_PREFIX.into(),
            ts: TS,
            block: 7,
        };
        let events = vec![
            ev(node("fra").label_hash(), &good),
            ev(node("nope").label_hash(), &good),
            ev(node("fra").label_hash(), "fra.1.00"),
        ];
        let params = MapParams::devnet();
        let (rows, evidence) = Validator::new(params.clone(), &reveals, &geo).validate_events(&events, &labels);
        let map = Map::build(&rows, TS + 10, &params);
        assert_eq!(map.rtt(c(ISTANBUL), &node("fra")), Some(12));
        assert_eq!(evidence.len(), 2);
        assert!(matches!(evidence[0].reason, Rejection::UnknownNode));
        assert!(matches!(evidence[1].reason, Rejection::BadToken));
        assert!(
            !reveals.check(&good, &node("fra"), "10.77.1.0/24"),
            "another network, another token"
        );
    }

    #[test]
    fn colluding_minority_cannot_move_the_median() {
        let ist = c(ISTANBUL);
        let mut rows = Vec::new();
        for a in 1..=3 {
            rows.push(row(a, ist, "n1", 250, false));
            rows.push(row(a, ist, "n2", 30, false));
        }
        for a in 4..=5 {
            rows.push(row(a, ist, "n1", 1, false)); // two humans claim node 1 is 1 ms away
            rows.push(row(a, ist, "n2", 30, false));
        }
        let map = Map::build(&rows, TS + 10, &MapParams::v1());
        let rank = map.rank(ist).unwrap();
        assert_eq!(rank.humans, 5);
        assert_eq!(rank.rings, 0);
        assert_eq!(rank.nodes, vec![(node("n2"), 30), (node("n1"), 250)]);
        assert!(map.covered.contains(&ist));
    }

    #[test]
    fn anchor_clamps_the_crowd_and_leaves_evidence() {
        let ist = c(ISTANBUL);
        let mut rows: Vec<ValidAttestation> =
            (1..=3).map(|a| row(a, ist, "n1", 30, false)).collect();
        rows.push(row(9, ist, "n1", 200, true));
        let map = Map::build(&rows, TS + 10, &MapParams::v1());
        assert_eq!(map.rtt(ist, &node("n1")), Some(200));
        assert_eq!(map.evidence.len(), 1);
        assert_eq!(
            map.evidence[0].auditors,
            vec![auditor(1), auditor(2), auditor(3)]
        );
        let mut rows: Vec<ValidAttestation> =
            (1..=3).map(|a| row(a, ist, "n1", 30, false)).collect();
        rows.push(row(9, ist, "n1", 60, true));
        let map = Map::build(&rows, TS + 10, &MapParams::v1());
        assert_eq!(map.rtt(ist, &node("n1")), Some(30));
        assert!(map.evidence.is_empty());
    }

    #[test]
    fn uncovered_cells_are_resolved_by_ring_search() {
        let ist = c(ISTANBUL);
        let rows: Vec<ValidAttestation> = (1..=3).map(|a| row(a, ist, "n1", 12, false)).collect();
        let map = Map::build(&rows, TS + 10, &MapParams::v1());
        let ring1 = geo::disk(ist, 1);
        let ring2: Vec<Cell> = geo::disk(ist, 2)
            .into_iter()
            .filter(|x| !ring1.contains(x))
            .collect();
        let two_away = ring2[0];
        let rank = map.rank(two_away).unwrap();
        assert_eq!((rank.rings, rank.humans), (2, 3));
        assert_eq!(map.rtt(two_away, &node("n1")), Some(12));
        let ring3 = geo::disk(ist, 3);
        let four_away = geo::disk(ist, 4)
            .into_iter()
            .find(|x| !ring3.contains(x))
            .unwrap();
        assert!(map.rank(four_away).is_none());
        assert!(!map.covered.contains(&two_away));
        let thin = Map::build(&rows[..2], TS + 10, &MapParams::v1());
        assert!(thin.ranks.is_empty() && thin.covered.is_empty());
        assert!(thin.nodes_seen.contains(&node("n1")));
    }

    #[test]
    fn window_and_location_estimate() {
        let ist = c(ISTANBUL);
        let fra = c(FRANKFURT);
        let mut rows = vec![
            row(1, ist, "n1", 10, false),
            row(1, fra, "n1", 40, false),
            row(1, fra, "n2", 8, false),
        ];
        rows.push(ValidAttestation {
            ts: TS - 3 * 86_400,
            ..row(1, ist, "n3", 5, false)
        }); // stale
        let map = Map::build(&rows, TS + 10, &MapParams::devnet());
        assert!(!map.nodes_seen.contains(&node("n3")));
        assert!(geo::distance_km(map.location(&node("n1")).unwrap(), ISTANBUL) < 200.0);
        assert!(geo::distance_km(map.location(&node("n2")).unwrap(), FRANKFURT) < 200.0);
        assert_eq!(
            map.rank(fra).unwrap().nodes,
            vec![(node("n2"), 8), (node("n1"), 40)]
        );
        let json = serde_json::to_string(&map).unwrap();
        let back: Map = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ranks, map.ranks);
    }

    /// The contract's rules, replayed: one vote per human per node per cell (newest wins, also across
    /// yesterday), medians as bucket midpoints over two days, ring search for cells without votes.
    #[test]
    fn votes_mirror_the_contract() {
        let ist = c(ISTANBUL);
        let fra = c(FRANKFURT);
        let labels = HashMap::from([
            (node("n1").label_hash(), node("n1")),
            (node("n2").label_hash(), node("n2")),
        ]);
        let members = Membership::new();
        let p = MapParams::devnet();
        let day = TS / 86_400 * 86_400;
        let events = vec![
            ev(1, ist, "n1", 10, day - 3_600), // yesterday
            ev(1, ist, "n1", 90, day + 60),    // the same human again today: replaces yesterday's vote
            ev(2, ist, "n1", 20, day + 120),
            ev(3, ist, "n1", 40, day + 180),
            ev(1, ist, "n2", 5, day + 240),
            ev(1, fra, "n2", 300, day + 300), // the same human in another cell: an independent vote
            ev(4, ist, "n2", 5, day - 3 * 86_400), // three days old: outside the window
        ];
        let map = Map::from_votes(&events, &members, &labels, day + 400, &p);
        let rank = map.rank(ist).unwrap();
        assert_eq!(rank.rings, 0);
        assert_eq!(rank.humans, 3);
        let who: Vec<(u8, u32)> = map.voters[&ist][&node("n1")]
            .iter()
            .map(|v| (v.attester.0[0], v.rtt_ms))
            .collect();
        assert_eq!(who, vec![(2, 20), (3, 40), (1, 90)], "the votes behind n1's median, by rtt");
        assert_eq!(map.voters[&fra][&node("n2")].len(), 1);
        assert!(!map.voters[&ist].contains_key(&node("n3")));
        // n1: votes 90, 20, 40 -> the lower-middle is 40, bucket 5 (40-48 ms), midpoint 44; n2: one vote, 5 ms -> 4
        assert_eq!(rank.nodes, vec![(node("n2"), 4), (node("n1"), 44)]);
        assert_eq!(map.rank(fra).unwrap().nodes, vec![(node("n2"), 252)]); // last bucket, open-ended
        assert!(map.covered.contains(&ist) && map.covered.contains(&fra));
        // a neighbour of Istanbul borrows its votes
        let near = geo::disk(ist, 1)[1];
        assert_eq!(map.rank(near).unwrap().rings, 1);
        assert_eq!(map.rank(near).unwrap().nodes[0], (node("n2"), 4));
        assert_eq!(histogram_median(&[0, 2, 1], 8), 12);
        assert_eq!(histogram_median(&[], 8), 0);
        // k_min: two votes needed and only one there -> not covered
        let strict = MapParams {
            k_min: 2,
            ..MapParams::devnet()
        };
        let map = Map::from_votes(&events[4..5], &members, &labels, day + 400, &strict);
        assert!(map.rank(ist).is_none());
    }

    #[test]
    fn median_is_lower_middle() {
        assert_eq!(median(&[]), 0);
        assert_eq!(median(&[5]), 5);
        assert_eq!(median(&[9, 1, 5]), 5);
        assert_eq!(median(&[1, 2, 3, 10]), 2);
    }
}
