//! Slot assignment (ARCHITECTURE.md §6): one set of `S` nodes per account, chosen by a deterministic
//! greedy k-median over the covered cells of the latency map, sticky day to day, one node per network
//! prefix, admission-capped. Everyone computes it from the same published inputs; nobody decides it.

use crate::map::Map;
use crate::token::prefix;
use crate::types::*;
use alloy::primitives::{keccak256, Address, B256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;

/// A node as the assignment sees it: identity from the registry, endpoint and class from its ENS records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    pub active: bool,
    pub entry_passed: bool,
    /// Highest conclusive class (100M when none has ever been conclusive).
    pub class: Class,
    pub stake: u128,
    pub ip: IpAddr,
    pub asn: Option<u32>,
    pub owner: Address,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub id: AccountId,
    /// `S`, the node-count tier.
    pub nodes: u8,
    pub class: Class,
    pub availability: Availability,
    pub min_node_stake: u128,
    pub operator_allowlist: Vec<Address>,
    pub operator_denylist: Vec<Address>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssignParams {
    /// A node may commit up to `k ×` its class across its slots.
    pub admission_k: u32,
    pub sticky_quality: f64,
    /// A kept member is rotated out only for a replacement that improves the objective by this fraction.
    pub rotation_gain: f64,
    /// Cost of a covered cell no member has a measurement for.
    pub uncovered_rtt_ms: u32,
}

impl Default for AssignParams {
    fn default() -> Self {
        Self {
            admission_k: 3,
            sticky_quality: 0.9,
            rotation_gain: 0.10,
            uncovered_rtt_ms: 10_000,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub seed: B256,
    pub sets: BTreeMap<AccountId, Vec<NodeId>>,
}

pub struct Yesterday<'a> {
    pub assignment: &'a Assignment,
    pub quality: &'a HashMap<(AccountId, NodeId), f64>,
}

/// A node nobody has measured yet is still eligible (the entry audit proved it is real); it costs
/// `uncovered_rtt_ms` in every covered cell, so measured nodes win whenever they exist.
pub fn eligible(n: &NodeInfo, a: &AccountInfo, _map: &Map) -> bool {
    n.active
        && n.entry_passed
        && n.class >= a.class
        && n.stake >= a.min_node_stake
        && !a.operator_denylist.contains(&n.owner)
        && (a.operator_allowlist.is_empty() || a.operator_allowlist.contains(&n.owner))
}

pub fn assign(
    seed: B256,
    nodes: &[NodeInfo],
    accounts: &[AccountInfo],
    map: &Map,
    yesterday: Option<&Yesterday>,
    p: &AssignParams,
) -> Assignment {
    let by_id: BTreeMap<&NodeId, &NodeInfo> = nodes.iter().map(|n| (&n.id, n)).collect();
    let covered: Vec<Cell> = map.covered.iter().copied().collect();
    let mut committed: BTreeMap<NodeId, u32> = BTreeMap::new();

    let mut order: Vec<&AccountInfo> = accounts.iter().collect();
    order.sort_by_key(|a| keccak256([seed.as_slice(), a.id.0.as_bytes()].concat()));

    // kept members register their capacity first, so stickiness never depends on the seeded order
    let mut kept: BTreeMap<AccountId, Vec<NodeId>> = BTreeMap::new();
    if let Some(y) = yesterday {
        for a in &order {
            let Some(prev) = y.assignment.sets.get(&a.id) else {
                continue;
            };
            let keep: Vec<NodeId> = prev
                .iter()
                .filter(|n| by_id.get(n).is_some_and(|info| eligible(info, a, map)))
                .filter(|n| {
                    y.quality
                        .get(&(a.id.clone(), (*n).clone()))
                        .copied()
                        .unwrap_or(0.0)
                        >= p.sticky_quality
                })
                .take(a.nodes as usize)
                .cloned()
                .collect();
            for n in &keep {
                *committed.entry(n.clone()).or_default() += a.class.mbps();
            }
            kept.insert(a.id.clone(), keep);
        }
    }

    let mut sets = BTreeMap::new();
    for a in order {
        let mut set: Vec<NodeId> = kept.remove(&a.id).unwrap_or_default();
        let s = a.nodes as usize;
        let rtt = |cell: Cell, n: &NodeId| map.rtt(cell, n).unwrap_or(p.uncovered_rtt_ms) as u64;
        let cost = |members: &[NodeId]| -> u64 {
            covered
                .iter()
                .map(|c| {
                    members
                        .iter()
                        .map(|m| rtt(*c, m))
                        .min()
                        .unwrap_or(p.uncovered_rtt_ms as u64)
                })
                .sum()
        };
        let admissible =
            |n: &NodeInfo, members: &[NodeId], committed: &BTreeMap<NodeId, u32>| -> bool {
                eligible(n, a, map)
                    && !members.contains(&n.id)
                    && !members.iter().any(|m| prefix(by_id[m].ip) == prefix(n.ip))
                    && committed.get(&n.id).copied().unwrap_or(0) + a.class.mbps()
                        <= p.admission_k * n.class.mbps()
            };

        while set.len() < s {
            let best = nodes
                .iter()
                .filter(|n| admissible(n, &set, &committed))
                .map(|n| {
                    let mut trial = set.clone();
                    trial.push(n.id.clone());
                    let same_asn = n.asn.is_some() && set.iter().any(|m| by_id[m].asn == n.asn);
                    (cost(&trial), same_asn, n.id.clone())
                })
                .min();
            let Some((_, _, id)) = best else { break };
            *committed.entry(id.clone()).or_default() += a.class.mbps();
            set.push(id);
        }

        // at most one kept member rotates out, and only for a clear improvement
        if let Some(prev) = yesterday.and_then(|y| y.assignment.sets.get(&a.id)) {
            let base = cost(&set);
            let mut best: Option<(u64, NodeId, NodeId)> = None; // (cost, out, in)
            for out in set
                .iter()
                .filter(|m| prev.contains(m))
                .cloned()
                .collect::<Vec<_>>()
            {
                let without: Vec<NodeId> = set.iter().filter(|m| **m != out).cloned().collect();
                for n in nodes
                    .iter()
                    .filter(|n| n.id != out && admissible(n, &without, &committed))
                {
                    let mut trial = without.clone();
                    trial.push(n.id.clone());
                    let candidate = (cost(&trial), out.clone(), n.id.clone());
                    if best.as_ref().is_none_or(|b| candidate < *b) {
                        best = Some(candidate);
                    }
                }
            }
            if let Some((c, out, inn)) = best {
                if base > 0 && (base - c.min(base)) as f64 > p.rotation_gain * base as f64 {
                    set.retain(|m| *m != out);
                    *committed.entry(out).or_default() -= a.class.mbps();
                    *committed.entry(inn.clone()).or_default() += a.class.mbps();
                    set.push(inn);
                }
            }
        }

        set.sort();
        sets.insert(a.id.clone(), set);
    }
    Assignment { seed, sets }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::fixtures::*;
    use crate::map::{MapParams, ValidAttestation};
    use proptest::prelude::*;
    use proptest::strategy::ValueTree;
    use std::net::Ipv4Addr;

    const TS: u64 = 1_788_480_000;

    fn nid(s: &str) -> NodeId {
        NodeId::new(s)
    }

    fn node(label: &str, ip: &str, class: Class, asn: Option<u32>) -> NodeInfo {
        NodeInfo {
            id: nid(label),
            active: true,
            entry_passed: true,
            class,
            stake: 1_000_000_000,
            ip: ip.parse().unwrap(),
            asn,
            owner: Address::ZERO,
        }
    }

    fn account(label: &str, nodes: u8, class: Class) -> AccountInfo {
        AccountInfo {
            id: AccountId::new(label),
            nodes,
            class,
            availability: Availability::P99,
            min_node_stake: 0,
            operator_allowlist: vec![],
            operator_denylist: vec![],
        }
    }

    fn row(cell: Cell, label: &str, rtt: u32) -> ValidAttestation {
        ValidAttestation {
            auditor: AuditorId(Address::from([1u8; 20])),
            cell,
            node: nid(label),
            rtt_ms: rtt,
            hit_ttfb_ms: None,
            ts: TS,
            anchor: false,
        }
    }

    /// Three cities, three nodes: `ist` in Istanbul, `fra` in Frankfurt, `tok` in Tokyo.
    fn city_map() -> Map {
        let (ist, fra, tok) = (c(ISTANBUL), c(FRANKFURT), c(TOKYO));
        let rows = vec![
            row(ist, "ist", 5),
            row(fra, "ist", 50),
            row(tok, "ist", 300),
            row(ist, "fra", 50),
            row(fra, "fra", 5),
            row(tok, "fra", 250),
            row(ist, "tok", 300),
            row(fra, "tok", 250),
            row(tok, "tok", 5),
        ];
        Map::build(&rows, TS + 10, &MapParams::devnet())
    }

    #[test]
    fn greedy_picks_coverage() {
        let nodes = vec![
            node("ist", "10.77.16.1", Class::M100, Some(1)),
            node("fra", "10.77.32.1", Class::M100, Some(2)),
            node("tok", "10.77.48.1", Class::M100, Some(3)),
        ];
        let a = assign(
            B256::ZERO,
            &nodes,
            &[account("acme", 2, Class::M100)],
            &city_map(),
            None,
            &AssignParams::default(),
        );
        // Frankfurt first (lowest single-node cost), then Tokyo covers the far cell
        assert_eq!(
            a.sets[&AccountId::new("acme")],
            vec![nid("fra"), nid("tok")]
        );
        let a = assign(
            B256::ZERO,
            &nodes,
            &[account("acme", 3, Class::M100)],
            &city_map(),
            None,
            &AssignParams::default(),
        );
        assert_eq!(
            a.sets[&AccountId::new("acme")],
            vec![nid("fra"), nid("ist"), nid("tok")]
        );
    }

    #[test]
    fn constraints_hold() {
        let map = city_map();
        // `tok` shares Frankfurt's network, otherwise attractive: never together with `fra`; `ghost` has no
        // measurements and is only ever the last resort
        let nodes = vec![
            node("ist", "10.77.16.1", Class::M100, None),
            node("fra", "10.77.32.1", Class::M100, None),
            node("tok", "10.77.32.9", Class::G1, None),
            node("ghost", "10.77.64.1", Class::M100, None),
        ];
        let acc = account("acme", 2, Class::M100);
        let a = assign(
            B256::ZERO,
            &nodes,
            std::slice::from_ref(&acc),
            &map,
            None,
            &AssignParams::default(),
        );
        assert_eq!(a.sets[&acc.id], vec![nid("fra"), nid("ist")]);
        let acc3 = account("acme", 3, Class::M100);
        let a = assign(
            B256::ZERO,
            &nodes,
            std::slice::from_ref(&acc3),
            &map,
            None,
            &AssignParams::default(),
        );
        let set = &a.sets[&acc3.id];
        assert!(!(set.contains(&nid("fra")) && set.contains(&nid("tok"))));
        assert_eq!(
            set,
            &vec![nid("fra"), nid("ghost"), nid("ist")],
            "the unmeasured node fills the last slot"
        );
        // class gate: a 1G account only takes 1G nodes
        let g = account("big", 2, Class::G1);
        let a = assign(
            B256::ZERO,
            &nodes,
            std::slice::from_ref(&g),
            &map,
            None,
            &AssignParams::default(),
        );
        assert_eq!(a.sets[&g.id], vec![nid("tok")]);
        // admission: a 100M node holds at most three 100M accounts
        let many: Vec<AccountInfo> = (0..5)
            .map(|i| account(&format!("a{i}"), 1, Class::M100))
            .collect();
        let one = vec![node("ist", "10.77.16.1", Class::M100, None)];
        let a = assign(
            B256::ZERO,
            &one,
            &many,
            &map,
            None,
            &AssignParams::default(),
        );
        assert_eq!(a.sets.values().filter(|s| !s.is_empty()).count(), 3);
    }

    #[test]
    fn sticky_and_rotation() {
        let map = city_map();
        let nodes = vec![
            node("ist", "10.77.16.1", Class::M100, None),
            node("fra", "10.77.32.1", Class::M100, None),
            node("tok", "10.77.48.1", Class::M100, None),
        ];
        let acc = account("acme", 2, Class::M100);
        let p = AssignParams::default();
        // yesterday: the poor set {ist, fra}; both served well, so both are kept unless a rotation clearly helps
        let prev = Assignment {
            seed: B256::ZERO,
            sets: BTreeMap::from([(acc.id.clone(), vec![nid("fra"), nid("ist")])]),
        };
        let good = HashMap::from([
            ((acc.id.clone(), nid("ist")), 1.0),
            ((acc.id.clone(), nid("fra")), 1.0),
        ]);
        let y = Yesterday {
            assignment: &prev,
            quality: &good,
        };
        let today = assign(
            B256::ZERO,
            &nodes,
            std::slice::from_ref(&acc),
            &map,
            Some(&y),
            &p,
        );
        // one rotation drops the cost from 260 to 60; {fra, tok} and {ist, tok} tie, and ties break by label
        assert_eq!(today.sets[&acc.id], vec![nid("ist"), nid("tok")]);
        // a member with poor quality is not kept at all
        let poor = HashMap::from([
            ((acc.id.clone(), nid("ist")), 0.5),
            ((acc.id.clone(), nid("fra")), 1.0),
        ]);
        let y = Yesterday {
            assignment: &prev,
            quality: &poor,
        };
        let today = assign(
            B256::ZERO,
            &nodes,
            std::slice::from_ref(&acc),
            &map,
            Some(&y),
            &p,
        );
        assert_eq!(today.sets[&acc.id], vec![nid("fra"), nid("tok")]);
        // no rotation when the gain is small: raise the bar and the kept set survives
        let y = Yesterday {
            assignment: &prev,
            quality: &good,
        };
        let strict = AssignParams {
            rotation_gain: 0.99,
            ..p.clone()
        };
        let today = assign(
            B256::ZERO,
            &nodes,
            std::slice::from_ref(&acc),
            &map,
            Some(&y),
            &strict,
        );
        assert_eq!(today.sets[&acc.id], vec![nid("fra"), nid("ist")]);
    }

    fn arb_node(id: u64) -> impl Strategy<Value = NodeInfo> {
        (0u8..2, 0u8..2, any::<bool>(), 0u32..3, 1u8..3).prop_map(
            move |(x, cls, active, asn, third)| NodeInfo {
                id: NodeId::new(format!("n{id}")),
                active,
                entry_passed: true,
                class: if cls == 0 { Class::M100 } else { Class::G1 },
                stake: 1_000,
                ip: IpAddr::V4(Ipv4Addr::new(10, x, third, id as u8)),
                asn: (asn > 0).then_some(asn),
                owner: Address::ZERO,
            },
        )
    }

    fn arb_rows(node_ids: Vec<NodeId>) -> impl Strategy<Value = Vec<ValidAttestation>> {
        let cells = [c(ISTANBUL), c(FRANKFURT), c(TOKYO)];
        let per_node: Vec<_> = node_ids
            .into_iter()
            .map(move |id| {
                proptest::collection::vec(proptest::option::of(5u32..300), 3).prop_map(
                    move |rtts| {
                        rtts.into_iter()
                            .zip(cells)
                            .filter_map(|(r, cell)| r.map(|r| row(cell, &id.0, r)))
                            .collect::<Vec<_>>()
                    },
                )
            })
            .collect();
        per_node.prop_map(|v| v.into_iter().flatten().collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn invariants(
            nodes in (3usize..9).prop_flat_map(|n| (1..=n as u64).map(arb_node).collect::<Vec<_>>()),
            sizes in proptest::collection::vec(1u8..4, 1..4),
            classes in proptest::collection::vec(any::<bool>(), 3),
            seed_byte in any::<u8>(),
            qualities in proptest::collection::vec(any::<bool>(), 32),
        ) {
            let ids: Vec<NodeId> = nodes.iter().map(|n| n.id.clone()).collect();
            let rows_strategy = arb_rows(ids);
            let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::default());
            let rows = rows_strategy.new_tree(&mut runner).unwrap().current();
            let map = Map::build(&rows, TS + 10, &MapParams::devnet());
            let accounts: Vec<AccountInfo> = sizes.iter().enumerate()
                .map(|(i, s)| account(&format!("acc{i}"), *s, if classes[i % 3] { Class::G1 } else { Class::M100 }))
                .collect();
            let p = AssignParams::default();
            let seed = B256::from([seed_byte; 32]);
            let first = assign(seed, &nodes, &accounts, &map, None, &p);
            prop_assert_eq!(&first, &assign(seed, &nodes, &accounts, &map, None, &p), "deterministic");

            // yesterday = a run under another seed, with random qualities
            let prev = assign(B256::from([seed_byte.wrapping_add(1); 32]), &nodes, &accounts, &map, None, &p);
            let mut quality = HashMap::new();
            for (i, (a, set)) in prev.sets.iter().enumerate() {
                for (j, n) in set.iter().enumerate() {
                    quality.insert((a.clone(), n.clone()), if qualities[(i * 4 + j) % 32] { 1.0 } else { 0.5 });
                }
            }
            let y = Yesterday { assignment: &prev, quality: &quality };
            let today = assign(seed, &nodes, &accounts, &map, Some(&y), &p);

            for out in [&first, &today] {
                let mut committed: BTreeMap<NodeId, u32> = BTreeMap::new();
                for a in &accounts {
                    let set = &out.sets[&a.id];
                    prop_assert!(set.len() <= a.nodes as usize);
                    let prefixes: std::collections::BTreeSet<String> =
                        set.iter().map(|n| prefix(nodes.iter().find(|x| x.id == *n).unwrap().ip)).collect();
                    prop_assert_eq!(prefixes.len(), set.len(), "one node per network");
                    for n in set {
                        let info = nodes.iter().find(|x| x.id == *n).unwrap();
                        prop_assert!(eligible(info, a, &map), "member is eligible");
                        *committed.entry(n.clone()).or_default() += a.class.mbps();
                    }
                }
                for (n, mbps) in committed {
                    let info = nodes.iter().find(|x| x.id == n).unwrap();
                    prop_assert!(mbps <= p.admission_k * info.class.mbps(), "admission cap");
                }
            }
            // sticky: of yesterday's good, still-eligible members at most one is gone
            for a in &accounts {
                let good: Vec<&NodeId> = prev.sets[&a.id].iter()
                    .filter(|n| quality[&(a.id.clone(), (*n).clone())] >= p.sticky_quality)
                    .collect();
                let gone = good.iter().filter(|n| !today.sets[&a.id].contains(n)).count();
                prop_assert!(gone <= 1, "at most one rotation per set per day");
            }
        }
    }
}
