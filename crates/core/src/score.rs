//! The scoring function (ARCHITECTURE.md §8): from published inputs to quality, SLA multipliers, bills,
//! pools, payouts, cumulative Merkle leaves and evidence. Pure and deterministic: the aggregator, the
//! countersigner and `recompute` all run exactly this, and anything not in the inputs cannot reach money.

use crate::assign::{AccountInfo, Assignment};
use crate::map;
use crate::merkle;
use crate::types::*;
use alloy::primitives::{Address, B256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreParams {
    pub throughput_floor_mbps: f64,
    /// Only fetches at least this large count as per-stream throughput samples.
    pub throughput_min_bytes: u64,
    pub sla_floor: f64,
    pub auditor_share: f64,
    pub resolver_share: f64,
    pub protocol_share: f64,
    pub wilson_z: f64,
    /// A timeout or 5xx counts against the node only if another member served the asset inside this window.
    pub attribution_window_s: u64,
    /// Distinct auditors that must see wrong bytes before integrity drops to zero.
    pub integrity_auditors: usize,
    pub outlier_min_probes: usize,
    pub outlier_ratio: f64,
    pub outlier_min_gap: f64,
    pub attestation_pay_cap: usize,
    /// A member counts as delivered when its uptime is above this.
    pub delivered_uptime: f64,
}

impl Default for ScoreParams {
    fn default() -> Self {
        Self {
            throughput_floor_mbps: 25.0,
            throughput_min_bytes: 1_000_000,
            sla_floor: 0.5,
            auditor_share: 0.10,
            resolver_share: 0.05,
            protocol_share: 0.05,
            wilson_z: 1.96,
            attribution_window_s: 600,
            integrity_auditors: 2,
            outlier_min_probes: 10,
            outlier_ratio: 3.0,
            outlier_min_gap: 0.2,
            attestation_pay_cap: 50,
            delivered_uptime: 0.5,
        }
    }
}

/// A probe result after the aggregator checked its token: `served_by` comes from the token, `target`
/// from the schedule (absent for landing probes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidProbe {
    pub auditor: AuditorId,
    pub probe: ProbeType,
    pub account: AccountId,
    pub asset: String,
    pub target: Option<NodeId>,
    pub served_by: Option<NodeId>,
    pub ok: bool,
    pub token_ok: bool,
    pub failure: Option<FailureClass>,
    pub metrics: ProbeMetrics,
    pub ts: u64,
}

/// The daily coordinated class probe, aggregated per node by the aggregator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassProbe {
    pub node: NodeId,
    pub total_mbps: f64,
    /// False when the auditors' own uplinks were the bottleneck.
    pub conclusive: bool,
}

/// Who receives money: nodes through the `addr(60)` record of their name, auditors and resolver operators
/// through the account that holds their role, the protocol through the treasury.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Payees {
    pub node_payout: BTreeMap<NodeId, Address>,
    pub treasury: Address,
}

/// What carries over from period to period: the cumulative leaves and the unpaid pools.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cumulative {
    pub payout: BTreeMap<Address, u128>,
    pub bill: BTreeMap<AccountId, u128>,
    pub rollover: BTreeMap<AccountId, u128>,
}

pub struct ScoreInput<'a> {
    pub period: u64,
    pub assignment: &'a Assignment,
    pub accounts: &'a [AccountInfo],
    /// List price per account per period, micro-USDC.
    pub list_price: &'a BTreeMap<AccountId, u128>,
    pub probes: &'a [ValidProbe],
    /// Origin digests by (account, asset path).
    pub digests: &'a BTreeMap<(AccountId, String), AssetDigest>,
    pub attestations_per_auditor: &'a BTreeMap<AuditorId, usize>,
    pub map_evidence: &'a [map::Evidence],
    pub class_probes: &'a [ClassProbe],
    pub prev_class: &'a BTreeMap<NodeId, Class>,
    pub conforming_resolvers: &'a [ResolverId],
    pub payees: &'a Payees,
    pub prev: &'a Cumulative,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quality {
    pub uptime: f64,
    pub integrity: f64,
    pub throughput: f64,
    pub quality: f64,
    pub valid_up: usize,
    pub failures: usize,
    pub paid: u128,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Multiplier {
    pub availability: f64,
    pub s_availability: f64,
    pub s_nodes: f64,
    pub s_throughput: f64,
    pub mult: f64,
    pub list: u128,
    pub bill: u128,
    pub credit: u128,
    pub pool: u128,
    pub paid: u128,
    pub rollover_next: u128,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ScoreEvidence {
    IntegrityMismatch {
        account: AccountId,
        node: NodeId,
        asset: String,
        auditors: Vec<AuditorId>,
    },
    InvalidToken {
        auditor: AuditorId,
        count: usize,
    },
    OutlierAuditor {
        auditor: AuditorId,
        account: AccountId,
        node: NodeId,
        rate: f64,
        others: f64,
    },
    Unmeasured {
        account: AccountId,
        node: NodeId,
    },
    SpoofedAttestation(map::Evidence),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreOutput {
    pub period: u64,
    pub quality: BTreeMap<AccountId, BTreeMap<NodeId, Quality>>,
    pub multipliers: BTreeMap<AccountId, Multiplier>,
    /// Paid this period, by address.
    pub payouts: BTreeMap<Address, u128>,
    pub auditor_pay: BTreeMap<AuditorId, u128>,
    pub resolver_pay: BTreeMap<ResolverId, u128>,
    pub protocol_fee: u128,
    pub cumulative: Cumulative,
    pub payout_leaves: Vec<(Address, u128)>,
    pub bill_leaves: Vec<(AccountId, u128)>,
    pub payout_root: B256,
    pub bill_root: B256,
    pub total_cum_payout: u128,
    pub total_cum_bill: u128,
    /// Tomorrow's eligibility class per probed node; `None` = failed even the lowest class.
    pub class_qualified: BTreeMap<NodeId, Option<Class>>,
    pub evidence: Vec<ScoreEvidence>,
}

/// `amount × f`, floored; zero for a non-positive factor.
pub fn mul(amount: u128, f: f64) -> u128 {
    if f <= 0.0 {
        0
    } else {
        (amount as f64 * f).floor() as u128
    }
}

/// Wilson score interval for `ok` successes in `n` trials.
pub fn wilson_bounds(ok: usize, n: usize, z: f64) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let n = n as f64;
    let p = ok as f64 / n;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denom;
    let half = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denom;
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

fn median_f(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Failure attribution (§7): connect, TLS and wrong bytes are the node's; a timeout or 5xx is the node's
/// only when another member served the same asset around the same time; DNS failures are nobody's here.
fn node_failure(x: &ValidProbe, probes: &[&ValidProbe], p: &ScoreParams) -> bool {
    match x.failure {
        Some(FailureClass::Connect | FailureClass::Tls | FailureClass::WrongBytes) => true,
        Some(FailureClass::Timeout | FailureClass::Http5xx) => probes.iter().any(|y| {
            y.ok && y.account == x.account
                && y.asset == x.asset
                && y.served_by.is_some()
                && y.served_by != x.target
                && y.ts.abs_diff(x.ts) <= p.attribution_window_s
        }),
        Some(FailureClass::Dns) | None => false,
    }
}

/// Auditors whose failure rate on a target is far above every other auditor's are suspended for the day.
fn outliers(
    probes: &[&ValidProbe],
    p: &ScoreParams,
    evidence: &mut Vec<ScoreEvidence>,
) -> BTreeSet<AuditorId> {
    let mut stats: BTreeMap<(AccountId, NodeId), BTreeMap<AuditorId, (usize, usize)>> =
        BTreeMap::new();
    for x in probes {
        let Some(t) = &x.target else { continue };
        let e = stats
            .entry((x.account.clone(), t.clone()))
            .or_default()
            .entry(x.auditor)
            .or_default();
        e.0 += 1;
        if !x.ok {
            e.1 += 1;
        }
    }
    let mut out = BTreeSet::new();
    for ((account, node), by_auditor) in stats {
        let rates: Vec<(AuditorId, f64)> = by_auditor
            .iter()
            .filter(|(_, (n, _))| *n >= p.outlier_min_probes)
            .map(|(a, (n, f))| (*a, *f as f64 / *n as f64))
            .collect();
        if rates.len() < 3 {
            continue;
        }
        for (a, rate) in &rates {
            let mut others: Vec<f64> = rates
                .iter()
                .filter(|(b, _)| b != a)
                .map(|(_, r)| *r)
                .collect();
            let med = median_f(&mut others);
            if *rate > med * p.outlier_ratio && rate - med >= p.outlier_min_gap {
                out.insert(*a);
                evidence.push(ScoreEvidence::OutlierAuditor {
                    auditor: *a,
                    account: account.clone(),
                    node: node.clone(),
                    rate: *rate,
                    others: med,
                });
            }
        }
    }
    out
}

/// Integrity (§8): zero for the day when enough auditors saw bytes matching neither digest while another
/// member served matching bytes for the same asset.
fn integrity_of(
    node: &NodeId,
    account: &AccountId,
    idx: &[usize],
    probes: &[&ValidProbe],
    digests: &BTreeMap<(AccountId, String), AssetDigest>,
    p: &ScoreParams,
    evidence: &mut Vec<ScoreEvidence>,
) -> f64 {
    let matches = |d: &AssetDigest, sha: &str| {
        sha == d.sha256_current || d.sha256_previous.as_deref() == Some(sha)
    };
    let mut by_asset: BTreeMap<&str, BTreeSet<AuditorId>> = BTreeMap::new();
    for &i in idx {
        let x = probes[i];
        if !(x.ok && x.served_by.as_ref() == Some(node)) {
            continue;
        }
        let (Some(sha), Some(d)) = (
            &x.metrics.sha256,
            digests.get(&(account.clone(), x.asset.clone())),
        ) else {
            continue;
        };
        if !matches(d, sha) {
            by_asset.entry(&x.asset).or_default().insert(x.auditor);
        }
    }
    for (asset, auditors) in by_asset {
        if auditors.len() < p.integrity_auditors {
            continue;
        }
        let d = &digests[&(account.clone(), asset.to_string())];
        let other_served_matching = idx.iter().any(|&i| {
            let y = probes[i];
            y.ok && y.served_by.is_some()
                && y.served_by.as_ref() != Some(node)
                && y.asset == asset
                && y.metrics.sha256.as_deref().is_some_and(|s| matches(d, s))
        });
        if other_served_matching {
            evidence.push(ScoreEvidence::IntegrityMismatch {
                account: account.clone(),
                node: node.clone(),
                asset: asset.to_string(),
                auditors: auditors.into_iter().collect(),
            });
            return 0.0;
        }
    }
    1.0
}

pub fn score(inp: &ScoreInput, p: &ScoreParams) -> ScoreOutput {
    let mut evidence = Vec::new();

    // 1. an "up" without a valid token is invalid and unpaid
    let mut invalid: BTreeMap<AuditorId, usize> = BTreeMap::new();
    let mut probes: Vec<&ValidProbe> = Vec::new();
    for x in inp.probes {
        if x.ok && !x.token_ok {
            *invalid.entry(x.auditor).or_default() += 1;
        } else {
            probes.push(x);
        }
    }
    for (auditor, count) in invalid {
        evidence.push(ScoreEvidence::InvalidToken { auditor, count });
    }

    // 2. outlier auditors are suspended for the day
    let suspended = outliers(&probes, p, &mut evidence);
    probes.retain(|x| !suspended.contains(&x.auditor));

    // 3. which failures are the node's
    let attributable: Vec<bool> = probes
        .iter()
        .map(|x| !x.ok && node_failure(x, &probes, p))
        .collect();

    let total_list: u128 = inp
        .accounts
        .iter()
        .map(|a| inp.list_price.get(&a.id).copied().unwrap_or(0))
        .sum();
    let mut quality_out = BTreeMap::new();
    let mut multipliers = BTreeMap::new();
    let mut payouts: BTreeMap<Address, u128> = BTreeMap::new();
    let mut cumulative = inp.prev.clone();

    for a in inp.accounts {
        let set = inp.assignment.sets.get(&a.id).cloned().unwrap_or_default();
        let s = a.nodes as usize;
        let list = inp.list_price.get(&a.id).copied().unwrap_or(0);
        let idx: Vec<usize> = (0..probes.len())
            .filter(|&i| probes[i].account == a.id)
            .collect();

        // 4. quality per member
        let mut q: BTreeMap<NodeId, Quality> = BTreeMap::new();
        let mut all_tp: Vec<f64> = Vec::new();
        for n in &set {
            let valid_up = idx
                .iter()
                .filter(|&&i| probes[i].ok && probes[i].served_by.as_ref() == Some(n))
                .count();
            let failures = idx
                .iter()
                .filter(|&&i| attributable[i] && probes[i].target.as_ref() == Some(n))
                .count();
            let uptime = if valid_up + failures == 0 {
                evidence.push(ScoreEvidence::Unmeasured {
                    account: a.id.clone(),
                    node: n.clone(),
                });
                0.0
            } else {
                valid_up as f64 / (valid_up + failures) as f64
            };
            let integrity = integrity_of(n, &a.id, &idx, &probes, inp.digests, p, &mut evidence);
            let mut tp: Vec<f64> = idx
                .iter()
                .filter_map(|&i| {
                    let x = probes[i];
                    (x.ok
                        && x.served_by.as_ref() == Some(n)
                        && x.probe == ProbeType::Targeted
                        && x.metrics.bytes.unwrap_or(0) >= p.throughput_min_bytes)
                        .then_some(x.metrics.kbps)
                        .flatten()
                        .map(|k| k as f64 / 1000.0)
                })
                .collect();
            let throughput = if tp.is_empty() {
                1.0
            } else {
                (median_f(&mut tp) / p.throughput_floor_mbps).min(1.0)
            };
            all_tp.extend(tp);
            let quality = uptime * integrity * throughput;
            q.insert(
                n.clone(),
                Quality {
                    uptime,
                    integrity,
                    throughput,
                    quality,
                    valid_up,
                    failures,
                    paid: 0,
                },
            );
        }

        // 5. SLA multiplier: credits only for statistically significant shortfalls
        let landing_ok = idx
            .iter()
            .filter(|&&i| probes[i].probe == ProbeType::Landing && probes[i].ok)
            .count();
        let landing_fail = idx
            .iter()
            .filter(|&&i| probes[i].probe == ProbeType::Landing && attributable[i])
            .count();
        let n_l = landing_ok + landing_fail;
        let availability = if n_l == 0 {
            1.0
        } else {
            landing_ok as f64 / n_l as f64
        };
        let target = a.availability.target();
        let s_availability = if n_l > 0 && wilson_bounds(landing_ok, n_l, p.wilson_z).1 < target {
            ((target - availability) / (1.0 - target)).max(0.0)
        } else {
            0.0
        };
        let delivered = q.values().filter(|x| x.uptime > p.delivered_uptime).count();
        let s_nodes = if s == 0 {
            0.0
        } else {
            (1.0 - delivered as f64 / s as f64).max(0.0)
        };
        let s_throughput = if all_tp.is_empty() {
            0.0
        } else {
            let m = median_f(&mut all_tp);
            if m < p.throughput_floor_mbps {
                1.0 - m / p.throughput_floor_mbps
            } else {
                0.0
            }
        };
        let factor = |sd: f64| (1.0 - 0.5 * sd).max(p.sla_floor);
        let mult = factor(s_availability)
            .min(factor(s_nodes))
            .min(factor(s_throughput));
        let bill = mul(list, mult);
        let credit = list - bill;

        // 6. the node pool and its slots
        let shares = p.auditor_share + p.resolver_share + p.protocol_share;
        let pool = mul(list, mult - shares) + inp.prev.rollover.get(&a.id).copied().unwrap_or(0);
        let share = if s == 0 { 0 } else { pool / s as u128 };
        let mut paid_total = 0u128;
        for (n, qq) in q.iter_mut() {
            let paid = mul(share, qq.quality);
            match inp.payees.node_payout.get(n) {
                Some(addr) if paid > 0 => {
                    *payouts.entry(*addr).or_default() += paid;
                    qq.paid = paid;
                    paid_total += paid;
                }
                _ => {}
            }
        }
        let rollover_next = pool - paid_total;
        multipliers.insert(
            a.id.clone(),
            Multiplier {
                availability,
                s_availability,
                s_nodes,
                s_throughput,
                mult,
                list,
                bill,
                credit,
                pool,
                paid: paid_total,
                rollover_next,
            },
        );
        cumulative.rollover.insert(a.id.clone(), rollover_next);
        *cumulative.bill.entry(a.id.clone()).or_default() += bill;
        quality_out.insert(a.id.clone(), q);
    }

    // 7. auditor pool by valid units: probes plus capped attestations, paid to the auditor's own account
    let auditor_pool = mul(total_list, p.auditor_share);
    let mut units: BTreeMap<AuditorId, u128> = BTreeMap::new();
    for x in &probes {
        *units.entry(x.auditor).or_default() += 1;
    }
    for (auditor, n) in inp.attestations_per_auditor {
        if !suspended.contains(auditor) {
            *units.entry(*auditor).or_default() += (*n).min(p.attestation_pay_cap) as u128;
        }
    }
    let total_units: u128 = units.values().sum();
    let mut auditor_pay = BTreeMap::new();
    let mut paid_auditors = 0u128;
    for (auditor, u) in &units {
        let Some(pay) = (auditor_pool * u).checked_div(total_units) else {
            break;
        };
        if pay == 0 {
            continue;
        }
        *payouts.entry(auditor.0).or_default() += pay;
        auditor_pay.insert(*auditor, pay);
        paid_auditors += pay;
    }

    // 8. resolver pool, equal among conforming operators, paid to their accounts
    let resolver_pool = mul(total_list, p.resolver_share);
    let mut resolver_pay = BTreeMap::new();
    let mut paid_resolvers = 0u128;
    if !inp.conforming_resolvers.is_empty() {
        let each = resolver_pool / inp.conforming_resolvers.len() as u128;
        for r in inp.conforming_resolvers {
            if each > 0 {
                *payouts.entry(r.0).or_default() += each;
                resolver_pay.insert(*r, each);
                paid_resolvers += each;
            }
        }
    }

    // 9. the protocol fee takes its share plus whatever dust nobody could receive
    let protocol_fee = mul(total_list, p.protocol_share)
        + (auditor_pool - paid_auditors)
        + (resolver_pool - paid_resolvers);
    if protocol_fee > 0 {
        *payouts.entry(inp.payees.treasury).or_default() += protocol_fee;
    }

    // 10. cumulative leaves and roots
    for (addr, amount) in &payouts {
        *cumulative.payout.entry(*addr).or_default() += amount;
    }
    let payout_leaves: Vec<(Address, u128)> =
        cumulative.payout.iter().map(|(a, v)| (*a, *v)).collect();
    let bill_leaves: Vec<(AccountId, u128)> = cumulative
        .bill
        .iter()
        .map(|(a, v)| (a.clone(), *v))
        .collect();
    let payout_root = merkle::Tree::new(
        payout_leaves
            .iter()
            .map(|(a, v)| merkle::payout_leaf(*a, *v))
            .collect(),
    )
    .root();
    let bill_root = merkle::Tree::new(
        bill_leaves
            .iter()
            .map(|(a, v)| merkle::bill_leaf(a.bytes32(), *v))
            .collect(),
    )
    .root();

    // 11. class qualification for tomorrow: inconclusive keeps the highest conclusive class, 100M when none
    let mut class_qualified = BTreeMap::new();
    for cp in inp.class_probes {
        let q = if cp.conclusive {
            if cp.total_mbps >= 1000.0 {
                Some(Class::G1)
            } else if cp.total_mbps >= 100.0 {
                Some(Class::M100)
            } else {
                None
            }
        } else {
            Some(inp.prev_class.get(&cp.node).copied().unwrap_or(Class::M100))
        };
        class_qualified.insert(cp.node.clone(), q);
    }

    for e in inp.map_evidence {
        evidence.push(ScoreEvidence::SpoofedAttestation(e.clone()));
    }

    ScoreOutput {
        period: inp.period,
        quality: quality_out,
        multipliers,
        payouts,
        auditor_pay,
        resolver_pay,
        protocol_fee,
        total_cum_payout: cumulative.payout.values().sum(),
        total_cum_bill: cumulative.bill.values().sum(),
        cumulative,
        payout_leaves,
        bill_leaves,
        payout_root,
        bill_root,
        class_qualified,
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC: u128 = 1_000_000;

    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    fn nid(s: &str) -> NodeId {
        NodeId::new(s)
    }

    fn aud(n: u8) -> AuditorId {
        AuditorId(addr(n))
    }

    fn account() -> AccountInfo {
        AccountInfo {
            id: AccountId::new("acme"),
            nodes: 3,
            class: Class::M100,
            availability: Availability::P999,
            min_node_stake: 0,
            operator_allowlist: vec![],
            operator_denylist: vec![],
        }
    }

    fn probe(
        auditor: u8,
        probe: ProbeType,
        target: Option<&str>,
        served_by: Option<&str>,
        ok: bool,
        i: u64,
    ) -> ValidProbe {
        ValidProbe {
            auditor: aud(auditor),
            probe,
            account: AccountId::new("acme"),
            asset: "/a".into(),
            target: target.map(nid),
            served_by: served_by.map(nid),
            ok,
            token_ok: true,
            failure: if ok {
                None
            } else {
                Some(FailureClass::Connect)
            },
            metrics: ProbeMetrics::default(),
            ts: 1_788_480_000 + i * 60,
        }
    }

    fn payees() -> Payees {
        Payees {
            node_payout: BTreeMap::from([
                (nid("n1"), addr(1)),
                (nid("n2"), addr(2)),
                (nid("n3"), addr(3)),
            ]),
            treasury: addr(99),
        }
    }

    /// One good node, one dead node, one node serving wrong bytes, one lazy auditor.
    fn fixture() -> Vec<ValidProbe> {
        let mut v = Vec::new();
        for auditor in 11..=13 {
            for i in 0..10 {
                let mut good = probe(
                    auditor,
                    ProbeType::Targeted,
                    Some("n1"),
                    Some("n1"),
                    true,
                    i,
                );
                good.metrics = ProbeMetrics {
                    sha256: Some("cur".into()),
                    bytes: Some(2_000_000),
                    kbps: Some(40_000),
                    ..Default::default()
                };
                v.push(good);
                v.push(probe(
                    auditor,
                    ProbeType::Targeted,
                    Some("n2"),
                    None,
                    false,
                    i,
                ));
                let mut bad = probe(
                    auditor,
                    ProbeType::Targeted,
                    Some("n3"),
                    Some("n3"),
                    true,
                    i,
                );
                bad.metrics.sha256 = Some("evil".into());
                v.push(bad);
                v.push(probe(
                    auditor,
                    ProbeType::Landing,
                    None,
                    Some("n1"),
                    true,
                    i,
                ));
            }
        }
        for i in 0..10 {
            let mut lazy = probe(14, ProbeType::Targeted, Some("n1"), Some("n1"), true, i);
            lazy.token_ok = false;
            v.push(lazy);
        }
        v
    }

    fn run(probes: &[ValidProbe], prev: &Cumulative) -> ScoreOutput {
        let acc = account();
        let assignment = Assignment {
            seed: B256::ZERO,
            sets: BTreeMap::from([(acc.id.clone(), vec![nid("n1"), nid("n2"), nid("n3")])]),
        };
        let list = BTreeMap::from([(acc.id.clone(), 10 * USDC)]);
        let digests = BTreeMap::from([(
            (acc.id.clone(), "/a".to_string()),
            AssetDigest {
                path: "/a".into(),
                size: 2_000_000,
                sha256_current: "cur".into(),
                sha256_previous: Some("prev".into()),
                etag: None,
            },
        )]);
        let payees = payees();
        let inp = ScoreInput {
            period: 1,
            assignment: &assignment,
            accounts: std::slice::from_ref(&acc),
            list_price: &list,
            probes,
            digests: &digests,
            attestations_per_auditor: &BTreeMap::new(),
            map_evidence: &[],
            class_probes: &[
                ClassProbe {
                    node: nid("n1"),
                    total_mbps: 1200.0,
                    conclusive: true,
                },
                ClassProbe {
                    node: nid("n2"),
                    total_mbps: 40.0,
                    conclusive: true,
                },
                ClassProbe {
                    node: nid("n3"),
                    total_mbps: 300.0,
                    conclusive: false,
                },
            ],
            prev_class: &BTreeMap::from([(nid("n3"), Class::G1)]),
            conforming_resolvers: &[ResolverId(addr(21))],
            payees: &payees,
            prev,
        };
        score(&inp, &ScoreParams::default())
    }

    #[test]
    fn bad_node_dead_node_lazy_auditor() {
        let out = run(&fixture(), &Cumulative::default());
        let acme = AccountId::new("acme");
        let q = &out.quality[&acme];
        assert_eq!(
            (
                q[&nid("n1")].uptime,
                q[&nid("n1")].integrity,
                q[&nid("n1")].quality
            ),
            (1.0, 1.0, 1.0)
        );
        assert_eq!(q[&nid("n2")].quality, 0.0);
        assert_eq!((q[&nid("n3")].uptime, q[&nid("n3")].integrity), (1.0, 0.0));

        let m = &out.multipliers[&acme];
        assert_eq!(m.s_availability, 0.0);
        assert!((m.s_nodes - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(m.bill, 8_333_333);
        assert_eq!(m.credit, 1_666_667);
        assert_eq!(m.pool, 6_333_333);
        assert_eq!(q[&nid("n1")].paid, 2_111_111);
        assert_eq!(m.rollover_next, 4_222_222);

        assert_eq!(out.auditor_pay[&aud(11)], 333_333);
        assert!(!out.auditor_pay.contains_key(&aud(14)));
        assert_eq!(
            out.payouts[&addr(11)],
            333_333,
            "auditors are paid to their own account"
        );
        assert_eq!(out.resolver_pay[&ResolverId(addr(21))], 500_000);
        assert_eq!(out.payouts[&addr(21)], 500_000);
        assert_eq!(out.protocol_fee, 500_001);
        assert_eq!(out.payouts[&addr(1)], 2_111_111);
        assert_eq!(
            out.total_cum_payout,
            2_111_111 + 3 * 333_333 + 500_000 + 500_001
        );
        assert_eq!(out.total_cum_bill, 8_333_333);
        assert!(out.total_cum_payout <= out.total_cum_bill);

        assert!(out.evidence.iter().any(|e| matches!(e, ScoreEvidence::IntegrityMismatch { node, auditors, .. } if *node == nid("n3") && auditors.len() == 3)));
        assert!(out.evidence.iter().any(|e| matches!(e, ScoreEvidence::InvalidToken { auditor, count: 10 } if *auditor == aud(14))));

        assert_eq!(out.class_qualified[&nid("n1")], Some(Class::G1));
        assert_eq!(out.class_qualified[&nid("n2")], None);
        assert_eq!(out.class_qualified[&nid("n3")], Some(Class::G1));

        // roots verify with the contract's leaf encoding
        let tree = merkle::Tree::new(
            out.payout_leaves
                .iter()
                .map(|(a, v)| merkle::payout_leaf(*a, *v))
                .collect(),
        );
        assert_eq!(tree.root(), out.payout_root);
        let leaf = merkle::payout_leaf(addr(1), 2_111_111);
        assert!(merkle::verify(
            &tree.proof(leaf).unwrap(),
            out.payout_root,
            leaf
        ));
    }

    #[test]
    fn cumulative_carries_over() {
        let first = run(&fixture(), &Cumulative::default());
        let second = run(&fixture(), &first.cumulative);
        let acme = AccountId::new("acme");
        // rollover from day one joins day two's pool
        assert_eq!(second.multipliers[&acme].pool, 6_333_333 + 4_222_222);
        assert_eq!(second.cumulative.bill[&acme], 2 * 8_333_333);
        assert_eq!(
            second.cumulative.payout[&addr(1)],
            2_111_111 + (6_333_333 + 4_222_222) / 3
        );
        assert!(second.total_cum_payout <= second.total_cum_bill);
        assert_ne!(second.payout_root, first.payout_root);
    }

    #[test]
    fn credits_need_significance() {
        let mut probes = Vec::new();
        for i in 0..100 {
            probes.push(probe(11, ProbeType::Landing, None, Some("n1"), i != 0, i));
            probes.push(probe(
                11,
                ProbeType::Targeted,
                Some("n1"),
                Some("n1"),
                true,
                i,
            ));
        }
        // one connect failure in 100 landings is significant against 99.9%: Wilson upper bound 0.998
        let out = run(&probes, &Cumulative::default());
        let m = &out.multipliers[&AccountId::new("acme")];
        assert_eq!(m.s_availability, 9.0);
        assert_eq!(m.mult, 0.5);
        // no failures: no credit, however few the samples
        let clean: Vec<ValidProbe> = probes.iter().filter(|x| x.ok).cloned().collect();
        let out = run(&clean, &Cumulative::default());
        assert_eq!(out.multipliers[&AccountId::new("acme")].s_availability, 0.0);
    }

    #[test]
    fn timeouts_are_the_origins_unless_a_member_served() {
        let mut lone = probe(11, ProbeType::Targeted, Some("n1"), None, false, 0);
        lone.failure = Some(FailureClass::Timeout);
        let served = probe(11, ProbeType::Targeted, Some("n2"), Some("n2"), true, 1);
        assert!(!node_failure(&lone, &[&lone], &ScoreParams::default()));
        assert!(node_failure(
            &lone,
            &[&lone, &served],
            &ScoreParams::default()
        ));
        let mut late = served.clone();
        late.ts += 3600;
        assert!(!node_failure(
            &lone,
            &[&lone, &late],
            &ScoreParams::default()
        ));
    }

    #[test]
    fn outlier_auditor_is_suspended_and_unpaid() {
        let mut probes = fixture();
        for i in 0..10 {
            probes.push(probe(15, ProbeType::Targeted, Some("n1"), None, false, i));
            // n1 fails only for auditor 15
        }
        let out = run(&probes, &Cumulative::default());
        assert!(out.evidence.iter().any(
            |e| matches!(e, ScoreEvidence::OutlierAuditor { auditor, .. } if *auditor == aud(15))
        ));
        assert!(!out.auditor_pay.contains_key(&aud(15)));
        assert_eq!(out.quality[&AccountId::new("acme")][&nid("n1")].uptime, 1.0);
    }

    #[test]
    fn wilson_and_mul() {
        let (lo, hi) = wilson_bounds(99, 100, 1.96);
        assert!(lo < 0.99 && hi < 0.999 && hi > 0.99);
        assert!(
            wilson_bounds(100, 100, 1.96).1 > 1.0 - 1e-9,
            "a perfect score has no significant shortfall"
        );
        assert_eq!(wilson_bounds(0, 0, 1.96), (0.0, 1.0));
        assert_eq!(mul(10 * USDC, 0.5), 5 * USDC);
        assert_eq!(mul(10 * USDC, -0.1), 0);
    }
}
