//! The identity cache (ARCHITECTURE.md §3, §11): every role, node, stake, suspension, key reveal, cell
//! membership and attestation of the Dedan registry and the cells contract, replayed from their events. Envelope verifiers, the map builder and
//! the assignment read from here; nothing on a request path touches the chain.
//!
//! Roles are Enhanced Access Control bitmaps per resource. A node's resource is its label id with the
//! registry's EAC version in the low 32 bits, bumped when the name is removed, so a grant on an old version
//! never carries over. A human's resource is `getResource(keccak256("dedan:human" ‖ account))`, which has
//! no registry entry and therefore always carries version 1.

use crate::envelope::KeyLookup;
use crate::map::{AttestedEvent, RevealSet};
use crate::types::*;
use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::eips::BlockNumberOrTag;
use alloy::providers::Provider;
use alloy::rpc::types::{Filter, Log};
use alloy::sol;
use alloy::sol_types::SolEvent;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

sol! {
    #[derive(Debug)]
    event EACRolesChanged(uint256 indexed resource, address indexed account, uint256 oldRoleBitmap, uint256 newRoleBitmap);
    #[derive(Debug)]
    event NodeRegistered(bytes32 indexed labelHash, string label, address indexed owner, uint256 stake);
    #[derive(Debug)]
    event LabelUnregistered(uint256 indexed tokenId, address indexed sender);
    #[derive(Debug)]
    event NodeUnbonding(bytes32 indexed labelHash, uint64 unbondAt);
    #[derive(Debug)]
    event NodeSuspended(bytes32 indexed labelHash, bool suspended);
    #[derive(Debug)]
    event NodeSlashed(bytes32 indexed labelHash, uint256 amount, address to);
    #[derive(Debug)]
    event AccountStaked(address indexed account, uint256 amount);
    #[derive(Debug)]
    event AccountUnbonding(address indexed account, uint64 unbondAt);
    #[derive(Debug)]
    event AccountWithdrawn(address indexed account, uint256 amount);
    #[derive(Debug)]
    event AccountSuspended(address indexed account, bool suspended);
    #[derive(Debug)]
    event AccountSlashed(address indexed account, uint256 amount, address to);
    #[derive(Debug)]
    event MinStakeSet(uint8 kind, uint256 amount);
    #[derive(Debug)]
    event KeyRevealed(bytes32 indexed labelHash, uint64 indexed hour, bytes32 kHour);
    #[derive(Debug)]
    event Attested(address indexed attester, uint64 indexed cell, bytes32 indexed labelHash, uint32 rttMs, uint32 hitTtfbMs, bytes token, bytes prefix, uint64 ts);
    #[derive(Debug)]
    event Joined(uint64 indexed cell, bytes32 indexed labelHash);
    #[derive(Debug)]
    event Left(uint64 indexed cell, bytes32 indexed labelHash);
}

/// Role bits of `DedanRoles.sol`, one nybble each.
pub mod roles {
    use alloy::primitives::U256;

    pub fn bit(n: usize) -> U256 {
        U256::from(1u8) << n
    }
    pub fn node_operator() -> U256 {
        bit(40)
    }
    pub fn attester() -> U256 {
        bit(44)
    }
    pub fn auditor() -> U256 {
        bit(48)
    }
    pub fn resolver_operator() -> U256 {
        bit(52)
    }
    pub fn node_signer() -> U256 {
        bit(56)
    }
}

/// `StakeKind` of the registry.
pub const STAKE_NODE: u8 = 0;
pub const STAKE_AUDITOR: u8 = 1;
pub const STAKE_RESOLVER: u8 = 2;

/// The label id with the version bits cleared: the key the registry stores an entry under.
pub fn label_base(id: B256) -> B256 {
    let mut b = id.0;
    b[28..].fill(0);
    B256::from(b)
}

fn version_of(resource: U256) -> u32 {
    resource.as_limbs()[0] as u32
}

fn base_of(resource: U256) -> B256 {
    label_base(B256::from(resource.to_be_bytes::<32>()))
}

/// The resource a human's roles live on: `keccak256("dedan:human" ‖ account)` with version 1.
pub fn human_resource(account: Address) -> U256 {
    let h = label_base(keccak256(
        [b"dedan:human".as_slice(), account.as_slice()].concat(),
    ));
    U256::from_be_bytes(h.0) | U256::from(1u8)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistryEvent {
    RolesChanged {
        resource: U256,
        account: Address,
        new: U256,
    },
    NodeRegistered {
        label: NodeId,
        owner: Address,
        stake: u128,
    },
    LabelUnregistered {
        token_id: U256,
    },
    NodeUnbonding {
        label_hash: B256,
    },
    NodeSuspended {
        label_hash: B256,
        suspended: bool,
    },
    NodeSlashed {
        label_hash: B256,
        amount: u128,
    },
    AccountStaked {
        account: Address,
        amount: u128,
    },
    AccountUnbonding {
        account: Address,
    },
    AccountWithdrawn {
        account: Address,
    },
    AccountSuspended {
        account: Address,
        suspended: bool,
    },
    AccountSlashed {
        account: Address,
        amount: u128,
    },
    MinStakeSet {
        kind: u8,
        amount: u128,
    },
    KeyRevealed {
        label_hash: B256,
        hour: u64,
        k_hour: B256,
    },
    Attested(AttestedEvent),
    Joined {
        cell: Cell,
        label_hash: B256,
    },
    Left {
        cell: Cell,
        label_hash: B256,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeEntry {
    pub label: NodeId,
    pub owner: Address,
    pub stake: u128,
    pub suspended: bool,
    pub unbonding: bool,
    pub eac_version: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanEntry {
    pub operator: bool,
    pub attester: bool,
    pub auditor: bool,
    pub stake: u128,
    pub suspended: bool,
    pub unbonding: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Identity {
    /// Registered nodes by label.
    pub nodes: BTreeMap<NodeId, NodeEntry>,
    /// Version-cleared label id → label, for every label the registry ever named.
    pub labels: HashMap<B256, NodeId>,
    pub humans: BTreeMap<Address, HumanEntry>,
    pub resolver_operators: BTreeSet<Address>,
    pub min_stake: BTreeMap<u8, u128>,
    /// `Attested` events in chain order, the map's input.
    pub attested: Vec<AttestedEvent>,
    /// The nodes that joined each cell (`DedanCells.join`), the cell's members.
    pub members: BTreeMap<Cell, BTreeSet<NodeId>>,
    /// Last block folded in, and its timestamp: the chain's clock, which the map's window follows.
    pub block: u64,
    pub block_time: u64,
    versions: HashMap<B256, u32>,
    /// Signer keys by (label base, EAC version).
    signers: HashMap<(B256, u32), BTreeSet<Address>>,
    #[serde(skip)]
    reveals: RevealSet,
    #[serde(skip)]
    reveal_log: Vec<KeyReveal>,
}

pub type IdentityError = Box<dyn std::error::Error + Send + Sync>;

fn to_u128(v: U256) -> u128 {
    u128::try_from(v).unwrap_or(u128::MAX)
}

impl Identity {
    pub fn apply(&mut self, ev: &RegistryEvent) {
        match ev.clone() {
            RegistryEvent::RolesChanged {
                resource,
                account,
                new,
            } => {
                if resource == U256::ZERO {
                    if new & roles::resolver_operator() != U256::ZERO {
                        self.resolver_operators.insert(account);
                    } else {
                        self.resolver_operators.remove(&account);
                    }
                } else if resource == human_resource(account) {
                    let h = self.humans.entry(account).or_default();
                    h.operator = new & roles::node_operator() != U256::ZERO;
                    h.attester = new & roles::attester() != U256::ZERO;
                    h.auditor = new & roles::auditor() != U256::ZERO;
                } else {
                    let set = self
                        .signers
                        .entry((base_of(resource), version_of(resource)))
                        .or_default();
                    if new & roles::node_signer() != U256::ZERO {
                        set.insert(account);
                    } else {
                        set.remove(&account);
                    }
                }
            }
            RegistryEvent::NodeRegistered {
                label,
                owner,
                stake,
            } => {
                let base = label_base(label.label_hash());
                self.labels.insert(base, label.clone());
                let eac_version = self.versions.get(&base).copied().unwrap_or(0);
                self.nodes.insert(
                    label.clone(),
                    NodeEntry {
                        label,
                        owner,
                        stake,
                        suspended: false,
                        unbonding: false,
                        eac_version,
                    },
                );
            }
            RegistryEvent::LabelUnregistered { token_id } => {
                let base = base_of(token_id);
                *self.versions.entry(base).or_default() += 1;
                if let Some(label) = self.labels.get(&base).cloned() {
                    self.nodes.remove(&label);
                }
            }
            RegistryEvent::NodeUnbonding { label_hash } => {
                if let Some(n) = self.node_mut(label_hash) {
                    n.unbonding = true;
                }
            }
            RegistryEvent::NodeSuspended {
                label_hash,
                suspended,
            } => {
                if let Some(n) = self.node_mut(label_hash) {
                    n.suspended = suspended;
                }
            }
            RegistryEvent::NodeSlashed { label_hash, amount } => {
                if let Some(n) = self.node_mut(label_hash) {
                    n.stake = n.stake.saturating_sub(amount);
                }
            }
            RegistryEvent::AccountStaked { account, amount } => {
                self.humans.entry(account).or_default().stake += amount;
            }
            RegistryEvent::AccountUnbonding { account } => {
                self.humans.entry(account).or_default().unbonding = true;
            }
            RegistryEvent::AccountWithdrawn { account } => {
                let h = self.humans.entry(account).or_default();
                h.stake = 0;
                h.unbonding = false;
            }
            RegistryEvent::AccountSuspended { account, suspended } => {
                self.humans.entry(account).or_default().suspended = suspended;
            }
            RegistryEvent::AccountSlashed { account, amount } => {
                let h = self.humans.entry(account).or_default();
                h.stake = h.stake.saturating_sub(amount);
            }
            RegistryEvent::MinStakeSet { kind, amount } => {
                self.min_stake.insert(kind, amount);
            }
            RegistryEvent::KeyRevealed {
                label_hash,
                hour,
                k_hour,
            } => {
                if let Some(label) = self.labels.get(&label_base(label_hash)) {
                    let r = KeyReveal {
                        node_id: label.clone(),
                        hour,
                        k_hour,
                    };
                    self.reveals.add(&r);
                    self.reveal_log.push(r);
                }
            }
            RegistryEvent::Attested(e) => self.attested.push(e),
            RegistryEvent::Joined { cell, label_hash } => {
                if let Some(label) = self.labels.get(&label_base(label_hash)).cloned() {
                    self.members.entry(cell).or_default().insert(label);
                }
            }
            RegistryEvent::Left { cell, label_hash } => {
                if let Some(label) = self.labels.get(&label_base(label_hash)).cloned() {
                    if let Some(set) = self.members.get_mut(&cell) {
                        set.remove(&label);
                    }
                }
            }
        }
    }

    fn node_mut(&mut self, label_hash: B256) -> Option<&mut NodeEntry> {
        let label = self.labels.get(&label_base(label_hash))?.clone();
        self.nodes.get_mut(&label)
    }

    /// The label behind a `labelHash` (or token id, or resource) the registry emitted.
    pub fn label_for(&self, label_hash: B256) -> Option<&NodeId> {
        self.labels.get(&label_base(label_hash))
    }

    /// Full label hash → label, the table the map validator wants.
    pub fn labels_by_hash(&self) -> HashMap<B256, NodeId> {
        self.labels
            .values()
            .map(|l| (l.label_hash(), l.clone()))
            .collect()
    }

    /// The hot keys currently allowed to sign for a node.
    pub fn signers(&self, label: &NodeId) -> BTreeSet<Address> {
        let Some(n) = self.nodes.get(label) else {
            return BTreeSet::new();
        };
        self.signers
            .get(&(label_base(label.label_hash()), n.eac_version))
            .cloned()
            .unwrap_or_default()
    }

    /// The registry's `isNodeActive`: not suspended, not unbonding, and still holding the minimum stake (a
    /// slash below it takes the node out).
    pub fn node_active(&self, label: &NodeId) -> bool {
        let min = self.min_stake.get(&STAKE_NODE).copied().unwrap_or(0);
        self.nodes
            .get(label)
            .is_some_and(|n| !n.suspended && !n.unbonding && n.stake >= min)
    }

    pub fn can_attest(&self, account: Address) -> bool {
        self.humans
            .get(&account)
            .is_some_and(|h| h.attester && !h.suspended)
    }

    /// The registry's `isAuditor`: the role, not suspended, not unbonding (its stake is on its way out), and the
    /// minimum stake.
    pub fn can_probe(&self, account: Address) -> bool {
        let min = self.min_stake.get(&STAKE_AUDITOR).copied().unwrap_or(0);
        self.humans
            .get(&account)
            .is_some_and(|h| h.auditor && !h.suspended && !h.unbonding && h.stake >= min)
    }

    /// Every account that may run assigned probes right now, in address order.
    pub fn auditors(&self) -> Vec<Address> {
        self.humans
            .keys()
            .copied()
            .filter(|a| self.can_probe(*a))
            .collect()
    }

    /// The registry's `isResolverOperator`: the role, not suspended, and the resolver bond.
    pub fn is_resolver_operator(&self, account: Address) -> bool {
        let min = self.min_stake.get(&STAKE_RESOLVER).copied().unwrap_or(0);
        self.resolver_operators.contains(&account)
            && self
                .humans
                .get(&account)
                .map_or(min == 0, |h| !h.suspended && h.stake >= min)
    }

    /// Key reveals seen so far, the map's token checker.
    pub fn reveals(&self) -> &RevealSet {
        &self.reveals
    }

    /// Rebuilds the reveal set after deserialisation (it is not serialised itself).
    pub fn reveal_log(&self) -> &[KeyReveal] {
        &self.reveal_log
    }

    pub fn decode(log: &Log) -> Option<RegistryEvent> {
        let t = *log.topic0()?;
        let ev = if t == EACRolesChanged::SIGNATURE_HASH {
            let d = log.log_decode::<EACRolesChanged>().ok()?.inner.data;
            RegistryEvent::RolesChanged {
                resource: d.resource,
                account: d.account,
                new: d.newRoleBitmap,
            }
        } else if t == NodeRegistered::SIGNATURE_HASH {
            let d = log.log_decode::<NodeRegistered>().ok()?.inner.data;
            RegistryEvent::NodeRegistered {
                label: NodeId::new(d.label),
                owner: d.owner,
                stake: to_u128(d.stake),
            }
        } else if t == LabelUnregistered::SIGNATURE_HASH {
            let d = log.log_decode::<LabelUnregistered>().ok()?.inner.data;
            RegistryEvent::LabelUnregistered {
                token_id: d.tokenId,
            }
        } else if t == NodeUnbonding::SIGNATURE_HASH {
            let d = log.log_decode::<NodeUnbonding>().ok()?.inner.data;
            RegistryEvent::NodeUnbonding {
                label_hash: d.labelHash,
            }
        } else if t == NodeSuspended::SIGNATURE_HASH {
            let d = log.log_decode::<NodeSuspended>().ok()?.inner.data;
            RegistryEvent::NodeSuspended {
                label_hash: d.labelHash,
                suspended: d.suspended,
            }
        } else if t == NodeSlashed::SIGNATURE_HASH {
            let d = log.log_decode::<NodeSlashed>().ok()?.inner.data;
            RegistryEvent::NodeSlashed {
                label_hash: d.labelHash,
                amount: to_u128(d.amount),
            }
        } else if t == AccountStaked::SIGNATURE_HASH {
            let d = log.log_decode::<AccountStaked>().ok()?.inner.data;
            RegistryEvent::AccountStaked {
                account: d.account,
                amount: to_u128(d.amount),
            }
        } else if t == AccountUnbonding::SIGNATURE_HASH {
            let d = log.log_decode::<AccountUnbonding>().ok()?.inner.data;
            RegistryEvent::AccountUnbonding { account: d.account }
        } else if t == AccountWithdrawn::SIGNATURE_HASH {
            let d = log.log_decode::<AccountWithdrawn>().ok()?.inner.data;
            RegistryEvent::AccountWithdrawn { account: d.account }
        } else if t == AccountSuspended::SIGNATURE_HASH {
            let d = log.log_decode::<AccountSuspended>().ok()?.inner.data;
            RegistryEvent::AccountSuspended {
                account: d.account,
                suspended: d.suspended,
            }
        } else if t == AccountSlashed::SIGNATURE_HASH {
            let d = log.log_decode::<AccountSlashed>().ok()?.inner.data;
            RegistryEvent::AccountSlashed {
                account: d.account,
                amount: to_u128(d.amount),
            }
        } else if t == MinStakeSet::SIGNATURE_HASH {
            let d = log.log_decode::<MinStakeSet>().ok()?.inner.data;
            RegistryEvent::MinStakeSet {
                kind: d.kind,
                amount: to_u128(d.amount),
            }
        } else if t == KeyRevealed::SIGNATURE_HASH {
            let d = log.log_decode::<KeyRevealed>().ok()?.inner.data;
            RegistryEvent::KeyRevealed {
                label_hash: d.labelHash,
                hour: d.hour,
                k_hour: d.kHour,
            }
        } else if t == Attested::SIGNATURE_HASH {
            let d = log.log_decode::<Attested>().ok()?.inner.data;
            RegistryEvent::Attested(AttestedEvent {
                attester: d.attester,
                cell: Cell(d.cell),
                label_hash: d.labelHash,
                rtt_ms: d.rttMs,
                hit_ttfb_ms: d.hitTtfbMs,
                token: String::from_utf8_lossy(&d.token).into_owned(),
                prefix: String::from_utf8_lossy(&d.prefix).into_owned(),
                ts: d.ts,
                block: log.block_number.unwrap_or(0),
            })
        } else if t == Joined::SIGNATURE_HASH {
            let d = log.log_decode::<Joined>().ok()?.inner.data;
            RegistryEvent::Joined {
                cell: Cell(d.cell),
                label_hash: d.labelHash,
            }
        } else if t == Left::SIGNATURE_HASH {
            let d = log.log_decode::<Left>().ok()?.inner.data;
            RegistryEvent::Left {
                cell: Cell(d.cell),
                label_hash: d.labelHash,
            }
        } else {
            return None;
        };
        Some(ev)
    }

    /// Folds every event of `contracts` (the registry and the cells contract) up to `to_block` into the
    /// cache, and notes that block's time. Returns how many events were applied.
    /// ponytail: one `eth_getLogs` for the whole range; chunk it when a public RPC caps the range.
    pub async fn replay<P: Provider>(
        &mut self,
        provider: &P,
        contracts: &[Address],
        to_block: u64,
    ) -> Result<usize, IdentityError> {
        let from = if self.block == 0 && self.nodes.is_empty() && self.humans.is_empty() {
            0
        } else {
            self.block + 1
        };
        if from > to_block {
            return Ok(0);
        }
        let filter = Filter::new()
            .address(contracts.to_vec())
            .from_block(from)
            .to_block(to_block);
        let logs = provider.get_logs(&filter).await?;
        let mut applied = 0;
        for log in &logs {
            if let Some(ev) = Self::decode(log) {
                self.apply(&ev);
                applied += 1;
            }
        }
        self.block = to_block;
        self.block_time = block_time(provider, to_block).await?;
        Ok(applied)
    }
}

/// The timestamp of a block: the chain's clock.
pub async fn block_time<P: Provider>(provider: &P, number: u64) -> Result<u64, IdentityError> {
    Ok(provider
        .get_block_by_number(BlockNumberOrTag::Number(number))
        .await?
        .map(|b| b.header.timestamp)
        .unwrap_or(0))
}

impl KeyLookup for Identity {
    fn accepts(&self, signer: &str, address: Address) -> bool {
        let Some((kind, id)) = signer.split_once(':') else {
            return false;
        };
        match kind {
            "node" => self.signers(&NodeId::new(id)).contains(&address),
            "auditor" => id
                .parse::<Address>()
                .is_ok_and(|a| a == address && self.can_probe(a)),
            "attester" => id
                .parse::<Address>()
                .is_ok_and(|a| a == address && self.can_attest(a)),
            "resolver" => id
                .parse::<Address>()
                .is_ok_and(|a| a == address && self.is_resolver_operator(a)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    fn node_resource(label: &NodeId, version: u32) -> U256 {
        U256::from_be_bytes(label_base(label.label_hash()).0) | U256::from(version)
    }

    fn registered(label: &str, owner: u8) -> Vec<RegistryEvent> {
        let id = NodeId::new(label);
        vec![
            // the owner's roles, then the signer grant, then the registration, as the contract emits them
            RegistryEvent::RolesChanged {
                resource: node_resource(&id, 0),
                account: addr(owner),
                new: roles::bit(24),
            },
            RegistryEvent::RolesChanged {
                resource: node_resource(&id, 0),
                account: addr(100 + owner),
                new: roles::node_signer(),
            },
            RegistryEvent::NodeRegistered {
                label: id,
                owner: addr(owner),
                stake: 1_000,
            },
        ]
    }

    #[test]
    fn nodes_signers_and_versions() {
        let mut id = Identity::default();
        for e in registered("fra", 1) {
            id.apply(&e);
        }
        let fra = NodeId::new("fra");
        assert_eq!(id.signers(&fra), BTreeSet::from([addr(101)]));
        assert!(id.accepts("node:fra", addr(101)));
        assert!(
            !id.accepts("node:fra", addr(1)),
            "the owner is not a signer unless granted"
        );
        assert!(id.node_active(&fra));

        // a revoke, a suspension, then the name goes away and its version bumps
        id.apply(&RegistryEvent::RolesChanged {
            resource: node_resource(&fra, 0),
            account: addr(101),
            new: U256::ZERO,
        });
        assert!(id.signers(&fra).is_empty());
        id.apply(&RegistryEvent::RolesChanged {
            resource: node_resource(&fra, 0),
            account: addr(102),
            new: roles::node_signer(),
        });
        id.apply(&RegistryEvent::NodeSuspended {
            label_hash: fra.label_hash(),
            suspended: true,
        });
        assert!(!id.node_active(&fra));
        let token_id = U256::from_be_bytes(label_base(fra.label_hash()).0) | U256::from(3u8);
        id.apply(&RegistryEvent::LabelUnregistered { token_id });
        assert!(!id.nodes.contains_key(&fra));
        assert!(id.signers(&fra).is_empty());

        // re-registered by someone else: version 1, fresh signer set
        let mut again = registered("fra", 2);
        for e in &mut again {
            if let RegistryEvent::RolesChanged { resource, .. } = e {
                *resource = node_resource(&fra, 1);
            }
        }
        for e in &again {
            id.apply(e);
        }
        assert_eq!(id.nodes[&fra].eac_version, 1);
        assert_eq!(id.signers(&fra), BTreeSet::from([addr(102)]));
        assert!(
            !id.accepts("node:fra", addr(101)),
            "an old version's key is dead"
        );
        assert!(id.node_active(&fra));
    }

    #[test]
    fn humans_and_root_roles() {
        let mut id = Identity::default();
        let a = addr(7);
        id.apply(&RegistryEvent::MinStakeSet {
            kind: STAKE_AUDITOR,
            amount: 100,
        });
        id.apply(&RegistryEvent::RolesChanged {
            resource: human_resource(a),
            account: a,
            new: roles::attester(),
        });
        assert!(id.can_attest(a) && !id.can_probe(a));
        assert!(id.accepts(&format!("attester:{a}"), a));
        id.apply(&RegistryEvent::RolesChanged {
            resource: human_resource(a),
            account: a,
            new: roles::attester() | roles::auditor(),
        });
        assert!(!id.can_probe(a), "auditor role without stake");
        id.apply(&RegistryEvent::AccountStaked {
            account: a,
            amount: 100,
        });
        assert!(id.can_probe(a));
        assert!(id.accepts(&format!("auditor:{a}"), a));
        id.apply(&RegistryEvent::AccountSuspended {
            account: a,
            suspended: true,
        });
        assert!(!id.can_attest(a) && !id.can_probe(a));
        id.apply(&RegistryEvent::AccountSuspended {
            account: a,
            suspended: false,
        });
        id.apply(&RegistryEvent::AccountUnbonding { account: a });
        id.apply(&RegistryEvent::AccountWithdrawn { account: a });
        assert!(id.can_attest(a) && !id.can_probe(a));

        let r = addr(8);
        id.apply(&RegistryEvent::RolesChanged {
            resource: U256::ZERO,
            account: r,
            new: roles::resolver_operator(),
        });
        assert!(id.is_resolver_operator(r) && id.accepts(&format!("resolver:{r}"), r));
        assert!(!id.accepts(&format!("resolver:{r}"), a));
        assert!(!id.accepts("cp", a));
    }

    #[test]
    fn reveals_and_attestations_are_collected() {
        let mut id = Identity::default();
        for e in registered("fra", 1) {
            id.apply(&e);
        }
        let fra = NodeId::new("fra");
        id.apply(&RegistryEvent::KeyRevealed {
            label_hash: fra.label_hash(),
            hour: 5,
            k_hour: B256::from([9u8; 32]),
        });
        id.apply(&RegistryEvent::KeyRevealed {
            label_hash: B256::from([1u8; 32]),
            hour: 5,
            k_hour: B256::ZERO,
        });
        assert_eq!(
            id.reveals().len(),
            1,
            "reveals for unknown labels are dropped"
        );
        assert_eq!(id.reveal_log()[0].node_id, fra);
        id.apply(&RegistryEvent::Attested(AttestedEvent {
            attester: addr(3),
            cell: Cell(1),
            label_hash: fra.label_hash(),
            rtt_ms: 12,
            hit_ttfb_ms: 0,
            token: "fra.1.00".into(),
            prefix: "10.77.0.0/24".into(),
            ts: 1,
            block: 2,
        }));
        assert_eq!(id.attested.len(), 1);
        assert_eq!(id.label_for(fra.label_hash()), Some(&fra));
    }

    /// Needs a devnet with the nodes registered: `make devnet`, then
    /// `cargo test -p dedan-core -- --ignored replays_the_devnet`.
    #[tokio::test]
    #[ignore]
    async fn replays_the_devnet() {
        use alloy::providers::ProviderBuilder;
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let dep: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{root}/contracts/deployments/devnet.json")).unwrap(),
        )
        .unwrap();
        let registry: Address = dep["dedanRegistry"].as_str().unwrap().parse().unwrap();
        let cells: Address = dep["dedanCells"].as_str().unwrap().parse().unwrap();
        let url = std::env::var("DEDAN_RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8545".into());
        let provider = ProviderBuilder::new().connect_http(url.parse().unwrap());
        let head = provider.get_block_number().await.unwrap();
        let mut id = Identity::default();
        let applied = id.replay(&provider, &[registry, cells], head).await.unwrap();
        assert!(
            applied >= 20,
            "min stakes, five nodes with roles, five attesters; got {applied}"
        );
        for city in ["frankfurt", "tokyo", "seoul", "newyork", "istanbul"] {
            let n = NodeId::new(city);
            assert!(id.node_active(&n), "{city}");
            assert_eq!(id.signers(&n).len(), 1, "{city} has one hot key");
            assert!(id.humans[&id.nodes[&n].owner].operator);
        }
        assert!(id.humans.values().filter(|h| h.attester).count() >= 5);
        assert_eq!(id.min_stake[&STAKE_AUDITOR], 100_000_000);
        assert_eq!(
            id.replay(&provider, &[registry, cells], head).await.unwrap(),
            0,
            "idempotent"
        );
    }

    /// The readers' mirror equals the contract: for every cell and member the contract holds votes for,
    /// `Map::from_votes` over the replayed events gives the same median as `DedanCells.median`.
    #[tokio::test]
    #[ignore = "needs a running devnet with votes (make demo or make demo-settlement)"]
    async fn mirror_matches_the_contract_medians() {
        use crate::map::{Map, MapParams};
        use alloy::providers::ProviderBuilder;
        use alloy::sol;
        sol! {
            #[sol(rpc)]
            interface IDedanCells {
                function median(uint64 cell, uint256 labelId) external view returns (uint32 ms, uint32 votes);
            }
        }
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let dep: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{root}/contracts/deployments/devnet.json")).unwrap(),
        )
        .unwrap();
        let registry: Address = dep["dedanRegistry"].as_str().unwrap().parse().unwrap();
        let cells: Address = dep["dedanCells"].as_str().unwrap().parse().unwrap();
        let url = std::env::var("DEDAN_RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8545".into());
        let provider = ProviderBuilder::new().connect_http(url.parse().unwrap());
        let head = provider.get_block_number().await.unwrap();
        let mut id = Identity::default();
        id.replay(&provider, &[registry, cells], head).await.unwrap();
        let map = Map::from_votes(
            &id.attested,
            &id.members,
            &id.labels_by_hash(),
            id.block_time,
            &MapParams::devnet(),
        );
        let contract = IDedanCells::new(cells, provider.clone());
        let mut compared = 0;
        for (cell, members) in &id.members {
            for node in members {
                let theirs = contract
                    .median(cell.0, U256::from_be_bytes(node.label_hash().0))
                    .call()
                    .await
                    .unwrap();
                let mine = map
                    .ranks
                    .get(cell)
                    .filter(|r| r.rings == 0)
                    .and_then(|r| r.nodes.iter().find(|(n, _)| n == node).map(|(_, ms)| *ms));
                if theirs.votes == 0 {
                    assert_eq!(mine, None, "{node} in cell {cell}: the contract has no votes");
                    continue;
                }
                assert_eq!(mine, Some(theirs.ms), "{node} in cell {cell} ({} votes)", theirs.votes);
                compared += 1;
            }
        }
        assert!(compared > 0, "no votes on the devnet yet");
    }
}
