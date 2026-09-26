//! The recompute contract (PLAN.md "Aggregator inputs and outputs"): everything a period's map and money
//! depend on is written to `inputs/`, and [`compute`] is a pure function from that directory to `outputs/`.
//! The aggregator, the countersigner and `recompute` run exactly this; anything not in `inputs/` cannot
//! reach money.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::sol_types::SolValue;
use dedan_core::assign::{assign, AccountInfo, AssignParams, Assignment, Yesterday};
use dedan_core::config::{self, Geo};
use dedan_core::ens::NodeRecord;
use dedan_core::envelope::{Envelope, KeyLookup, MAX_SKEW_S};
use dedan_core::identity::{Identity, RegistryEvent};
use dedan_core::map::{self, Map, MapParams, Rejection, TokenCheck, TokenStatus, Validator};
use dedan_core::merkle;
use dedan_core::schedule;
use dedan_core::score::{self, Cumulative, Payees, ScoreInput, ScoreOutput, ScoreParams, ValidProbe};
use dedan_core::settlement::{self, AccountState};
use dedan_core::token::Token;
use dedan_core::types::*;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub const SCORE_VERSION: u32 = 1;
pub const PRICE_TABLE_VERSION: u32 = 1;

/// `inputs/manifest.json`: the period's bounds, the chain state it was computed against, the parameters,
/// and the hash of every other input file. `inputs_hash` is the sha256 of this file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The settlement contract's period number.
    pub period: u64,
    /// The wall-clock period index the results were filed under.
    pub wall_index: u64,
    pub start: u64,
    pub end: u64,
    pub registry_block: u64,
    /// The timestamp of `registry_block`: the chain's clock, which the map's window follows.
    #[serde(default)]
    pub chain_time: u64,
    pub chain_id: u64,
    pub settlement: Address,
    pub governance: Address,
    pub price_table_version: u32,
    pub score_version: u32,
    pub period_secs: u64,
    pub sample_n: usize,
    pub map_k: usize,
    /// Auditors the schedule assigns per account; results from the others are not scored.
    #[serde(default = "default_auditors_per_account")]
    pub auditors_per_account: usize,
    pub prev_outputs_hash: String,
    pub files: BTreeMap<String, String>,
}

fn default_auditors_per_account() -> usize {
    5
}

/// A tier's daily list price in the on-chain table at `registry_block`, micro-USDC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierPrice {
    pub tier: Tier,
    pub per_day: u128,
}

/// What carries over from the previous period's outputs: cumulative leaves, the sets (for stickiness) and
/// the members' quality (the sticky threshold).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Prev {
    pub cumulative: Cumulative,
    pub assignment: Assignment,
    pub quality: Vec<(AccountId, NodeId, f64)>,
}

/// The whole `inputs/` directory in memory.
#[derive(Debug, Clone)]
pub struct Inputs {
    pub manifest: Manifest,
    pub events: Vec<RegistryEvent>,
    pub probes: Vec<Envelope<ProbeBatch>>,
    pub samples: Vec<AssetSample>,
    /// Account configs as the control plane relays them, signed by the account keys (verified in `compute`).
    pub configs: Vec<Envelope<AccountConfig>>,
    /// Every account created on the Settlement contract, as it stood at `registry_block`.
    pub accounts: Vec<AccountState>,
    /// The daily price of every tier the configs use, from the on-chain table at `registry_block`.
    pub prices: Vec<TierPrice>,
    pub nodes: BTreeMap<NodeId, NodeRecord>,
    pub geo: Geo,
    pub prev: Prev,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leaf<T> {
    pub who: T,
    pub cumulative: u128,
    pub proof: Vec<B256>,
}

/// `outputs/roots.json`: what `publishRoot` receives, its digest, the signatures gathered and the transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Roots {
    pub period: u64,
    pub wall_index: u64,
    pub inputs_hash: B256,
    pub registry_block: u64,
    pub price_table_version: u32,
    pub payout_root: B256,
    pub bill_root: B256,
    pub total_cum_payout: u128,
    pub total_cum_bill: u128,
    pub digest: B256,
    #[serde(default)]
    pub sigs: BTreeMap<String, String>,
    #[serde(default)]
    pub tx: Option<B256>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapOut {
    pub covered: usize,
    pub ranks: Vec<CellOut>,
    pub node_location: BTreeMap<NodeId, LatLon>,
    pub anchor_evidence: Vec<map::AnchorDisagreement>,
    pub rejected: Vec<map::Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellOut {
    pub cell: String,
    pub humans: usize,
    pub rings: u32,
    pub nodes: Vec<(NodeId, u32)>,
}

#[derive(Debug, Clone)]
pub struct Outputs {
    pub map: MapOut,
    pub assignment: Assignment,
    pub score: ScoreOutput,
    pub payout_leaves: Vec<Leaf<Address>>,
    pub bill_leaves: Vec<Leaf<AccountId>>,
    pub roots: Roots,
    pub next: Prev,
    /// Envelopes that failed verification, with the reason; they are in the inputs and count for nothing.
    pub rejected_batches: Vec<(String, String)>,
    pub valid_probes: usize,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn usdc(x: u128) -> String {
    format!("{}.{:06}", x / 1_000_000, x % 1_000_000)
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<(), Error> {
    fs::write(path, serde_json::to_vec_pretty(v)?)?;
    Ok(())
}

fn write_jsonl<T: Serialize>(path: &Path, rows: &[T]) -> Result<(), Error> {
    let mut out = String::new();
    for r in rows {
        out.push_str(&serde_json::to_string(r)?);
        out.push('\n');
    }
    fs::write(path, out)?;
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, Error> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, Error> {
    let text = fs::read_to_string(path)?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}

pub fn inputs_dir(dir: &Path) -> PathBuf {
    dir.join("inputs")
}

pub fn outputs_dir(dir: &Path) -> PathBuf {
    dir.join("outputs")
}

impl Inputs {
    /// Writes every file, then the manifest with their hashes. Returns the manifest as written.
    pub fn write(&mut self, dir: &Path) -> Result<Manifest, Error> {
        let d = inputs_dir(dir);
        fs::create_dir_all(&d)?;
        self.probes.sort_by_cached_key(|e| {
            (
                e.signer.clone(),
                e.ts,
                e.hash().map(|h| h.to_string()).unwrap_or_default(),
            )
        });
        write_jsonl(&d.join("registry-events.jsonl"), &self.events)?;
        write_jsonl(&d.join("probe-results.jsonl"), &self.probes)?;
        write_jsonl(&d.join("asset-samples.jsonl"), &self.samples)?;
        write_jsonl(&d.join("configs.jsonl"), &self.configs)?;
        write_json(&d.join("accounts.json"), &self.accounts)?;
        write_json(&d.join("prices.json"), &self.prices)?;
        write_json(&d.join("nodes.json"), &self.nodes)?;
        write_json(&d.join("geo.json"), &self.geo)?;
        write_json(&d.join("prev.json"), &self.prev)?;
        let mut files = BTreeMap::new();
        for name in [
            "registry-events.jsonl",
            "probe-results.jsonl",
            "asset-samples.jsonl",
            "configs.jsonl",
            "accounts.json",
            "prices.json",
            "nodes.json",
            "geo.json",
            "prev.json",
        ] {
            files.insert(name.to_owned(), sha256_hex(&fs::read(d.join(name))?));
        }
        self.manifest.files = files;
        write_json(&d.join("manifest.json"), &self.manifest)?;
        Ok(self.manifest.clone())
    }

    pub fn read(dir: &Path) -> Result<Self, Error> {
        let d = inputs_dir(dir);
        let manifest: Manifest = read_json(&d.join("manifest.json"))?;
        for (name, hash) in &manifest.files {
            let actual = sha256_hex(&fs::read(d.join(name))?);
            if actual != *hash {
                return Err(format!("{name}: sha256 {actual} differs from the manifest's {hash}").into());
            }
        }
        Ok(Self {
            events: read_jsonl(&d.join("registry-events.jsonl"))?,
            probes: read_jsonl(&d.join("probe-results.jsonl"))?,
            samples: read_jsonl(&d.join("asset-samples.jsonl"))?,
            configs: read_jsonl(&d.join("configs.jsonl"))?,
            accounts: read_json(&d.join("accounts.json"))?,
            prices: read_json(&d.join("prices.json"))?,
            nodes: read_json(&d.join("nodes.json"))?,
            geo: read_json(&d.join("geo.json"))?,
            prev: read_json(&d.join("prev.json"))?,
            manifest,
        })
    }

    /// `sha256(manifest.json)` as the contract carries it.
    pub fn inputs_hash(dir: &Path) -> Result<B256, Error> {
        let bytes = fs::read(inputs_dir(dir).join("manifest.json"))?;
        Ok(B256::from_slice(&Sha256::digest(&bytes)))
    }
}

/// `Settlement.digest(r)`: `keccak256(abi.encode(chainid, settlement, period, inputsHash, registryBlock,
/// priceTableVersion, payoutRoot, billRoot, totalCumPayout, totalCumBill))`.
pub fn digest(chain_id: u64, settlement: Address, r: &Roots) -> B256 {
    keccak256(
        (
            U256::from(chain_id),
            settlement,
            r.period,
            r.inputs_hash,
            r.registry_block,
            r.price_table_version,
            r.payout_root,
            r.bill_root,
            U256::from(r.total_cum_payout),
            U256::from(r.total_cum_bill),
        )
            .abi_encode(),
    )
}

/// The pure function: inputs to outputs.
pub fn compute(inp: &Inputs, inputs_hash: B256) -> Result<Outputs, Error> {
    let m = &inp.manifest;
    let mut identity = Identity::default();
    for e in &inp.events {
        identity.apply(e);
    }
    identity.block = m.registry_block;
    identity.block_time = m.chain_time;
    let labels = identity.labels_by_hash();
    let reveals = identity.reveals();

    // 0. the accounts: a config counts only with the signature of the account key on chain (§9), the newest
    //    version per account; the price is the tier's in the on-chain table
    let chain_accounts: BTreeMap<AccountId, &AccountState> =
        inp.accounts.iter().map(|a| (a.id.clone(), a)).collect();
    let mut configs: BTreeMap<AccountId, AccountConfig> = BTreeMap::new();
    let mut by_version: Vec<&Envelope<AccountConfig>> = inp.configs.iter().collect();
    by_version.sort_by_key(|e| e.payload.version);
    for env in by_version {
        let id = &env.payload.account_id;
        let key = chain_accounts.get(id).map(|a| a.key).unwrap_or(Address::ZERO);
        if settlement::verify_config(env, key).is_ok()
            && settlement::newer(configs.get(id).map(|c| c.version), env.payload.version)
        {
            configs.insert(id.clone(), env.payload.clone());
        }
    }
    let seed = schedule::seed_of(m.start);

    // the auditors the schedule assigned to each account (§7); results from anyone else are not scored
    let candidates = identity.auditors();
    let assigned: BTreeMap<AccountId, BTreeSet<Address>> = configs
        .keys()
        .map(|id| {
            let set = schedule::auditors_for(seed, id, &candidates, m.auditors_per_account);
            (id.clone(), set.into_iter().collect())
        })
        .collect();

    // 1. probe batches: a registered auditor's signature over exactly this content, signed inside this period
    //    and filed once, then the token check at the result's time
    let mut probes: Vec<ValidProbe> = Vec::new();
    let mut rejected_batches = Vec::new();
    let mut batches_seen: HashSet<(String, u64)> = HashSet::new();
    let mut tokens_seen: HashSet<(AuditorId, String)> = HashSet::new();
    for env in &inp.probes {
        let reason = match env.recover() {
            Err(e) => Some(e.to_string()),
            Ok(addr) if !identity.accepts(&env.signer, addr) => {
                Some(format!("{} holds no auditor role with stake", env.signer))
            }
            Ok(addr) if env.payload.auditor_id.0 != addr => Some("auditor id differs from the signer".into()),
            // a batch from another period cannot be slipped into this one's inputs
            Ok(_) if env.ts + MAX_SKEW_S < m.start || env.ts > m.end + MAX_SKEW_S => {
                Some(format!("signed at {}, outside the period {}..{}", env.ts, m.start, m.end))
            }
            Ok(_) if !batches_seen.insert((env.signer.clone(), env.nonce)) => {
                Some(format!("nonce {} filed twice", env.nonce))
            }
            Ok(_) => None,
        };
        if let Some(r) = reason {
            rejected_batches.push((env.signer.clone(), r));
            continue;
        }
        let auditor = env.payload.auditor_id;
        for r in &env.payload.results {
            let served_by = r
                .token
                .as_deref()
                .and_then(|t| t.parse::<Token>().ok())
                .map(|t| t.node_id);
            let status = match (&served_by, &r.token) {
                (Some(node), Some(t)) => reveals.check(t, node, r.ts),
                _ => TokenStatus::Invalid,
            };
            // the node never revealed that hour's key: the result proves nothing either way, and it is the
            // node's loss, not evidence against the auditor
            if r.ok && status == TokenStatus::NoKey {
                continue;
            }
            let token_ok = status == TokenStatus::Valid;
            // one real fetch backs one result: a token repeated by the same auditor counts once
            if let (true, Some(t)) = (token_ok, &r.token) {
                if !tokens_seen.insert((auditor, t.clone())) {
                    continue;
                }
            }
            let on_schedule = assigned.get(&r.account).is_some_and(|a| a.contains(&auditor.0));
            // off-schedule results are not scored; a forged "up" is still evidence
            if !on_schedule && !(r.ok && !token_ok) {
                continue;
            }
            probes.push(ValidProbe {
                auditor: env.payload.auditor_id,
                probe: r.probe,
                account: r.account.clone(),
                asset: r.asset.clone(),
                target: r.target.clone(),
                served_by,
                ok: r.ok,
                token_ok,
                failure: r.failure,
                metrics: r.metrics.clone(),
                ts: r.ts,
            });
        }
    }

    // 2. the contract's map, mirrored from its events; the token and location checks it cannot make give
    //    this period's paid attestations and rejections
    let params = MapParams {
        k_min: m.map_k,
        ..MapParams::v1()
    };
    let map = Map::from_votes(
        &identity.attested,
        &identity.members,
        &labels,
        m.chain_time,
        &params,
    );
    let mut v = Validator::new(params.clone(), reveals, &inp.geo);
    let (rows, rejected) = v.validate_events(&identity.attested, &labels);
    let in_period = |ts: u64| ts >= m.start && ts < m.end;
    // pay: at most `attestation_pay_cap` valid attestations per human per epoch (the day), counted in chain
    // order across every period of the epoch; a token repeated by the same human counts once
    let cap = ScoreParams::default().attestation_pay_cap;
    let mut per_epoch: HashMap<(AuditorId, u64), usize> = HashMap::new();
    let mut attestations_per_auditor: BTreeMap<AuditorId, usize> = BTreeMap::new();
    let mut attested_tokens: HashSet<(AuditorId, String)> = HashSet::new();
    for r in &rows {
        if !attested_tokens.insert((r.auditor, r.token.clone())) {
            continue;
        }
        let n = per_epoch.entry((r.auditor, params.epoch_of(r.ts))).or_default();
        if *n < cap && in_period(r.ts) {
            *attestations_per_auditor.entry(r.auditor).or_default() += 1;
        }
        *n += 1;
    }
    // a key the node never revealed is nobody's evidence
    let map_evidence: Vec<map::Evidence> = rejected
        .iter()
        .filter(|e| in_period(e.ts) && e.reason != Rejection::Unverifiable)
        .cloned()
        .collect();

    // 3. the slot sets, sticky on the previous period's
    let nodes = config::node_infos(&identity, &inp.nodes);
    let accounts: Vec<AccountInfo> = configs.values().map(AccountInfo::from_config).collect();
    let quality: HashMap<(AccountId, NodeId), f64> = inp
        .prev
        .quality
        .iter()
        .map(|(a, n, q)| ((a.clone(), n.clone()), *q))
        .collect();
    let yesterday = Yesterday {
        assignment: &inp.prev.assignment,
        quality: &quality,
    };
    let assignment = assign(
        seed,
        &nodes,
        &accounts,
        &map,
        Some(&yesterday),
        &AssignParams::default(),
    );

    // 4. scoring
    let list_price: BTreeMap<AccountId, u128> = configs
        .values()
        .map(|c| {
            let per_day = inp
                .prices
                .iter()
                .find(|p| p.tier == c.tier)
                .map(|p| p.per_day)
                .unwrap_or(0);
            (c.account_id.clone(), settlement::list_price(per_day, m.period_secs))
        })
        .collect();
    // what each account on chain can still be billed: its balance, minus the bills published before this
    // period and not yet applied, minus a withdrawal on its way out
    let balances: BTreeMap<AccountId, u128> = inp
        .accounts
        .iter()
        .map(|a| {
            let billed = inp.prev.cumulative.bill.get(&a.id).copied().unwrap_or(0);
            (a.id.clone(), a.effective_balance(billed))
        })
        .collect();
    let digests: BTreeMap<(AccountId, String), AssetDigest> = inp
        .samples
        .iter()
        .flat_map(|s| {
            s.assets
                .iter()
                .map(move |d| ((s.account_id.clone(), d.path.clone()), d.clone()))
        })
        .collect();
    let payees = Payees {
        node_payout: inp
            .nodes
            .iter()
            .filter(|(_, r)| r.payout != Address::ZERO)
            .map(|(n, r)| (n.clone(), r.payout))
            .collect(),
        treasury: m.governance,
    };
    let score = score::score(
        &ScoreInput {
            period: m.period,
            assignment: &assignment,
            accounts: &accounts,
            list_price: &list_price,
            probes: &probes,
            digests: &digests,
            attestations_per_auditor: &attestations_per_auditor,
            map_evidence: &map_evidence,
            class_probes: &[],
            prev_class: &BTreeMap::new(),
            conforming_resolvers: &[],
            payees: &payees,
            prev: &inp.prev.cumulative,
            balances: &balances,
        },
        &ScoreParams::default(),
    );

    // 5. leaves with proofs, roots, digest
    let payout_tree = merkle::Tree::new(
        score
            .payout_leaves
            .iter()
            .map(|(a, v)| merkle::payout_leaf(*a, *v))
            .collect(),
    );
    let bill_tree = merkle::Tree::new(
        score
            .bill_leaves
            .iter()
            .map(|(a, v)| merkle::bill_leaf(a.bytes32(), *v))
            .collect(),
    );
    let payout_leaves = score
        .payout_leaves
        .iter()
        .map(|(a, v)| Leaf {
            who: *a,
            cumulative: *v,
            proof: payout_tree
                .proof(merkle::payout_leaf(*a, *v))
                .unwrap_or_default(),
        })
        .collect();
    let bill_leaves = score
        .bill_leaves
        .iter()
        .map(|(a, v)| Leaf {
            who: a.clone(),
            cumulative: *v,
            proof: bill_tree
                .proof(merkle::bill_leaf(a.bytes32(), *v))
                .unwrap_or_default(),
        })
        .collect();
    let mut roots = Roots {
        period: m.period,
        wall_index: m.wall_index,
        inputs_hash,
        registry_block: m.registry_block,
        price_table_version: m.price_table_version,
        payout_root: score.payout_root,
        bill_root: score.bill_root,
        total_cum_payout: score.total_cum_payout,
        total_cum_bill: score.total_cum_bill,
        digest: B256::ZERO,
        sigs: BTreeMap::new(),
        tx: None,
    };
    roots.digest = digest(m.chain_id, m.settlement, &roots);

    let next = Prev {
        cumulative: score.cumulative.clone(),
        assignment: assignment.clone(),
        quality: score
            .quality
            .iter()
            .flat_map(|(a, by_node)| {
                by_node
                    .iter()
                    .map(move |(n, q)| (a.clone(), n.clone(), q.quality))
            })
            .collect(),
    };
    let map_out = MapOut {
        covered: map.covered.len(),
        ranks: map
            .ranks
            .iter()
            .map(|(c, r)| CellOut {
                cell: c.0.to_string(),
                humans: r.humans,
                rings: r.rings,
                nodes: r.nodes.clone(),
            })
            .collect(),
        node_location: map.node_location.clone(),
        anchor_evidence: map.evidence.clone(),
        rejected: map_evidence,
    };
    Ok(Outputs {
        map: map_out,
        assignment,
        valid_probes: probes.len(),
        score,
        payout_leaves,
        bill_leaves,
        roots,
        next,
        rejected_batches,
    })
}

impl Outputs {
    pub fn write(&self, dir: &Path) -> Result<(), Error> {
        let d = outputs_dir(dir);
        fs::create_dir_all(&d)?;
        write_json(&d.join("map.json"), &self.map)?;
        write_json(&d.join("assignment.json"), &self.assignment)?;
        write_json(&d.join("score.json"), &self.score)?;
        write_json(&d.join("evidence.json"), &self.score.evidence)?;
        write_json(&d.join("payout-leaves.json"), &self.payout_leaves)?;
        write_json(&d.join("bill-leaves.json"), &self.bill_leaves)?;
        write_json(&d.join("next.json"), &self.next)?;
        write_json(&d.join("rejected-batches.json"), &self.rejected_batches)?;
        write_json(&d.join("roots.json"), &self.roots)?;
        Ok(())
    }
}

pub fn read_roots(dir: &Path) -> Result<Roots, Error> {
    read_json(&outputs_dir(dir).join("roots.json"))
}

pub fn write_roots(dir: &Path, roots: &Roots) -> Result<(), Error> {
    write_json(&outputs_dir(dir).join("roots.json"), roots)
}

pub fn read_prev(dir: &Path) -> Result<Prev, Error> {
    read_json(&outputs_dir(dir).join("next.json"))
}

pub fn read_score(dir: &Path) -> Result<ScoreOutput, Error> {
    read_json(&outputs_dir(dir).join("score.json"))
}

pub fn read_leaves<T: DeserializeOwned>(dir: &Path, name: &str) -> Result<Vec<Leaf<T>>, Error> {
    read_json(&outputs_dir(dir).join(name))
}

pub fn read_samples(dir: &Path) -> Result<Vec<AssetSample>, Error> {
    read_jsonl(&inputs_dir(dir).join("asset-samples.jsonl"))
}

/// Recomputes a period from its inputs and compares with the roots on disk; the differences, if any.
pub fn check(dir: &Path) -> Result<(Outputs, Roots, Vec<String>), Error> {
    let inputs = Inputs::read(dir)?;
    let out = compute(&inputs, Inputs::inputs_hash(dir)?)?;
    let theirs = read_roots(dir)?;
    let mine = &out.roots;
    let mut diffs = Vec::new();
    let mut cmp = |what: &str, a: String, b: String| {
        if a != b {
            diffs.push(format!("{what}: recomputed {a}, published {b}"));
        }
    };
    cmp("period", mine.period.to_string(), theirs.period.to_string());
    cmp("inputs_hash", mine.inputs_hash.to_string(), theirs.inputs_hash.to_string());
    cmp("payout_root", mine.payout_root.to_string(), theirs.payout_root.to_string());
    cmp("bill_root", mine.bill_root.to_string(), theirs.bill_root.to_string());
    cmp("total_cum_payout", mine.total_cum_payout.to_string(), theirs.total_cum_payout.to_string());
    cmp("total_cum_bill", mine.total_cum_bill.to_string(), theirs.total_cum_bill.to_string());
    cmp("digest", mine.digest.to_string(), theirs.digest.to_string());
    Ok((out, theirs, diffs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_the_contract_encoding() {
        // abi.encode of ten static words is 320 bytes; the hash of the layout below was checked with cast
        let r = Roots {
            period: 1,
            wall_index: 0,
            inputs_hash: B256::from([1u8; 32]),
            registry_block: 7,
            price_table_version: 1,
            payout_root: B256::from([2u8; 32]),
            bill_root: B256::from([3u8; 32]),
            total_cum_payout: 100,
            total_cum_bill: 200,
            digest: B256::ZERO,
            sigs: BTreeMap::new(),
            tx: None,
        };
        let enc = (
            U256::from(31337u64),
            Address::from([9u8; 20]),
            r.period,
            r.inputs_hash,
            r.registry_block,
            r.price_table_version,
            r.payout_root,
            r.bill_root,
            U256::from(r.total_cum_payout),
            U256::from(r.total_cum_bill),
        )
            .abi_encode();
        assert_eq!(enc.len(), 320);
        assert_eq!(digest(31337, Address::from([9u8; 20]), &r), keccak256(enc));
    }

    /// The period's inputs are checked, not trusted: a batch signed in another period or filed twice counts for
    /// nothing, and a config counts only with the account key's signature.
    #[test]
    fn compute_checks_batches_and_configs() {
        use alloy::signers::local::PrivateKeySigner;
        use dedan_core::identity::{human_resource, roles, STAKE_AUDITOR};
        let auditor = PrivateKeySigner::from_slice(&[3u8; 32]).unwrap();
        let customer = PrivateKeySigner::from_slice(&[4u8; 32]).unwrap();
        let a = auditor.address();
        let acme = AccountId::new("acme");
        let file = |nodes: u8| -> dedan_core::config::AccountCfg {
            serde_json::from_str(&format!(
                r#"{{"label":"acme","hostnames":["www.images.com"],"nodes":{nodes},"class":"100M","availability":"99.9"}}"#
            ))
            .unwrap()
        };
        let good = Envelope::sign(file(6).to_config(1), acme.signer(), 10_000, 1, &customer).unwrap();
        // newer, but signed by someone else: if it counted, its tier (3 nodes) has no price and the list would be 0
        let forged = Envelope::sign(file(3).to_config(2), acme.signer(), 10_000, 2, &auditor).unwrap();
        let batch = |ts: u64, nonce: u64| {
            let b = ProbeBatch {
                auditor_id: AuditorId(a),
                period: 0,
                results: vec![],
            };
            Envelope::sign(b, AuditorId(a).signer(), ts, nonce, &auditor).unwrap()
        };
        let inputs = Inputs {
            manifest: Manifest {
                period: 1,
                wall_index: 0,
                start: 10_000,
                end: 10_120,
                registry_block: 1,
                chain_time: 10_100,
                chain_id: 31337,
                settlement: Address::ZERO,
                governance: Address::ZERO,
                price_table_version: 1,
                score_version: SCORE_VERSION,
                period_secs: 120,
                sample_n: 4,
                map_k: 1,
                auditors_per_account: 5,
                prev_outputs_hash: String::new(),
                files: BTreeMap::new(),
            },
            events: vec![
                RegistryEvent::MinStakeSet {
                    kind: STAKE_AUDITOR,
                    amount: 100,
                },
                RegistryEvent::RolesChanged {
                    resource: human_resource(a),
                    account: a,
                    new: roles::auditor(),
                },
                RegistryEvent::AccountStaked {
                    account: a,
                    amount: 100,
                },
            ],
            probes: vec![batch(10_010, 1), batch(10_020, 1), batch(5_000, 2)],
            samples: vec![],
            configs: vec![good, forged],
            accounts: vec![AccountState {
                id: acme.clone(),
                key: customer.address(),
                balance: 1_000_000_000,
                ..Default::default()
            }],
            prices: vec![TierPrice {
                tier: file(6).to_config(1).tier,
                per_day: 7_200_000_000,
            }],
            nodes: BTreeMap::new(),
            geo: Geo::default(),
            prev: Prev::default(),
        };
        let out = compute(&inputs, B256::ZERO).unwrap();
        let reasons: Vec<&str> = out.rejected_batches.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(reasons.len(), 2, "{reasons:?}");
        assert!(reasons.iter().any(|r| r.contains("filed twice")));
        assert!(reasons.iter().any(|r| r.contains("outside the period")));
        assert_eq!(out.score.multipliers[&acme].list, 10_000_000, "the account's own config, priced on chain");
        assert!(out.bill_leaves.iter().any(|l| l.who == acme));
    }

    #[test]
    fn usdc_formats_micro_amounts() {
        assert_eq!(usdc(7_500_000), "7.500000");
        assert_eq!(usdc(5), "0.000005");
    }
}
