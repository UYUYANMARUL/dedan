//! The probe schedule (ARCHITECTURE.md §7): which assets an account is sampled on, and what each auditor
//! probes when, all derived from a seed, so anyone can recompute who was supposed to probe what.

use crate::types::*;
use alloy::primitives::{keccak256, Address, B256};

/// The day's seed, the one the assignment uses too. ponytail: the first L1 block at or after the period
/// start once periods are hourly; a hash of the day is enough while the map moves daily.
pub fn seed_of(ts: u64) -> B256 {
    keccak256((ts / 86_400).to_be_bytes())
}

fn draw(seed: B256, parts: &[&[u8]]) -> u64 {
    let mut buf = seed.to_vec();
    for p in parts {
        buf.extend_from_slice(p);
        buf.push(0);
    }
    let h = keccak256(&buf);
    u64::from_be_bytes(h[..8].try_into().expect("8 bytes"))
}

/// The `n` assets of an account's manifest sampled for the day, without repeats, in draw order. The manifest
/// is the customer's own file: repeated lines count once, or a list with fewer distinct paths than `n` would
/// keep every auditor and the period close drawing forever.
pub fn sample_assets(seed: B256, account: &AccountId, manifest: &[String], n: usize) -> Vec<String> {
    let mut unique: Vec<&String> = Vec::new();
    for m in manifest {
        if !unique.contains(&m) {
            unique.push(m);
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut k = 0u64;
    while out.len() < n.min(unique.len()) {
        let i = draw(seed, &[account.0.as_bytes(), b"asset", &k.to_be_bytes()]) as usize
            % unique.len();
        if !out.contains(unique[i]) {
            out.push(unique[i].clone());
        }
        k += 1;
    }
    out
}

/// The auditors assigned to an account for the day (§7): every candidate ranked by
/// `hash(seed ‖ account ‖ auditor)`, the first `n` kept, so anyone can recompute who was supposed to probe
/// what and no auditor picks its own targets. `candidates` are the accounts that may probe (role and stake).
pub fn auditors_for(seed: B256, account: &AccountId, candidates: &[Address], n: usize) -> Vec<Address> {
    let mut ranked: Vec<(u64, Address)> = candidates
        .iter()
        .map(|a| (draw(seed, &[account.0.as_bytes(), b"auditor", a.as_slice()]), *a))
        .collect();
    ranked.sort();
    ranked.dedup_by_key(|(_, a)| *a);
    ranked.into_iter().take(n).map(|(_, a)| a).collect()
}

/// One scheduled probe: the asset, the slot member aimed at (`None` = a landing probe through the
/// auditor's own resolver), and how long to wait before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub asset: String,
    pub target: Option<NodeId>,
    pub delay_ms: u64,
}

/// Probe `k` of `auditor` on `account`: landing and targeted probes alternate, targeted ones walk the
/// members in turn; waits are exponential with mean `mean_s` (a Poisson process), so nodes cannot
/// predict them.
pub fn slot(
    seed: B256,
    account: &AccountId,
    auditor: AuditorId,
    k: u64,
    members: &[NodeId],
    sample: &[String],
    mean_s: f64,
) -> Slot {
    let who = auditor.0.as_slice();
    let asset = if sample.is_empty() {
        "/".to_owned()
    } else {
        let i = draw(seed, &[account.0.as_bytes(), who, b"pick", &k.to_be_bytes()]) as usize
            % sample.len();
        sample[i].clone()
    };
    let target = if k % 2 == 1 && !members.is_empty() {
        Some(members[(k / 2) as usize % members.len()].clone())
    } else {
        None
    };
    let u = draw(seed, &[account.0.as_bytes(), who, b"wait", &k.to_be_bytes()]);
    // uniform in (0, 1], so the logarithm is finite
    let u = (u as f64 + 1.0) / (u64::MAX as f64 + 2.0);
    Slot {
        asset,
        target,
        delay_ms: (-u.ln() * mean_s * 1000.0) as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;

    fn manifest() -> Vec<String> {
        (1..=10).map(|i| format!("/a/{i}")).collect()
    }

    #[test]
    fn sample_is_deterministic_and_distinct() {
        let seed = seed_of(1_790_000_000);
        let a = sample_assets(seed, &AccountId::new("acme"), &manifest(), 4);
        let b = sample_assets(seed, &AccountId::new("acme"), &manifest(), 4);
        assert_eq!(a, b);
        assert_eq!(a.len(), 4);
        let mut d = a.clone();
        d.dedup();
        assert_eq!(d.len(), 4);
        assert_eq!(sample_assets(seed, &AccountId::new("acme"), &manifest(), 50).len(), 10);
        assert_ne!(a, sample_assets(seed_of(1_790_100_000), &AccountId::new("acme"), &manifest(), 4));
        assert!(sample_assets(seed, &AccountId::new("acme"), &[], 4).is_empty());
    }

    #[test]
    fn slots_alternate_and_wait_a_random_time() {
        let seed = seed_of(1_790_000_000);
        let members = vec![NodeId::new("a"), NodeId::new("b"), NodeId::new("c")];
        let sample = vec!["/x".to_owned(), "/y".to_owned()];
        let me = AuditorId(Address::from([9u8; 20]));
        let slots: Vec<Slot> = (0..6)
            .map(|k| slot(seed, &AccountId::new("acme"), me, k, &members, &sample, 3.0))
            .collect();
        assert!(slots[0].target.is_none() && slots[2].target.is_none());
        assert_eq!(slots[1].target.as_deref_label(), Some("a"));
        assert_eq!(slots[3].target.as_deref_label(), Some("b"));
        assert_eq!(slots[5].target.as_deref_label(), Some("c"));
        let mean: f64 = slots.iter().map(|s| s.delay_ms as f64).sum::<f64>() / 6.0;
        assert!(mean > 0.0 && mean < 30_000.0);
        assert_eq!(slots[1], slot(seed, &AccountId::new("acme"), me, 1, &members, &sample, 3.0));
        assert_eq!(slot(seed, &AccountId::new("acme"), me, 1, &[], &[], 3.0).asset, "/");
    }

    trait Label {
        fn as_deref_label(&self) -> Option<&str>;
    }

    impl Label for Option<NodeId> {
        fn as_deref_label(&self) -> Option<&str> {
            self.as_ref().map(|n| n.0.as_str())
        }
    }

    #[test]
    fn repeated_manifest_lines_count_once() {
        let seed = B256::from([3u8; 32]);
        let acme = AccountId::new("acme");
        let twice = vec!["/a".to_string(), "/a".to_string()];
        assert_eq!(sample_assets(seed, &acme, &twice, 4), vec!["/a".to_string()]);
        let m: Vec<String> = ["/a", "/b", "/b", "/b"].iter().map(|x| x.to_string()).collect();
        let mut got = sample_assets(seed, &acme, &m, 4);
        got.sort();
        assert_eq!(got, vec!["/a".to_string(), "/b".to_string()]);
    }

    #[test]
    fn auditors_are_chosen_by_the_seed() {
        let seed = B256::from([5u8; 32]);
        let acme = AccountId::new("acme");
        let all: Vec<Address> = (1..=8u8).map(|i| Address::from([i; 20])).collect();
        let five = auditors_for(seed, &acme, &all, 5);
        assert_eq!(five.len(), 5);
        let mut shuffled = all.clone();
        shuffled.reverse();
        assert_eq!(auditors_for(seed, &acme, &shuffled, 5), five, "order of candidates does not matter");
        assert_ne!(auditors_for(B256::from([6u8; 32]), &acme, &all, 5), five, "another day, another draw");
        assert_eq!(auditors_for(seed, &acme, &all[..3], 5).len(), 3, "fewer candidates than the target: all");
    }
}
