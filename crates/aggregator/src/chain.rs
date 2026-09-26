//! The chain side of a period: the registry's events up to a block, the Settlement contract's state, and
//! `publishRoot` with the two signatures.

use alloy::primitives::{Address, Bytes, B256};
use alloy::providers::{DynProvider, Provider};
use alloy::rpc::types::Filter;
use alloy::sol;
use alloy::sol_types::SolEvent;
use dedan_core::identity::{Identity, RegistryEvent};
use dedan_core::settlement::{self, AccountState};
use dedan_core::types::{AccountId, Tier};

use crate::settle::{Roots, TierPrice};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

sol! {
    #[sol(rpc)]
    interface ISettlement {
        struct RootInput {
            uint64 period;
            bytes32 inputsHash;
            uint64 registryBlock;
            uint32 priceTableVersion;
            bytes32 payoutRoot;
            bytes32 billRoot;
            uint256 totalCumPayout;
            uint256 totalCumBill;
        }
        function lastPeriod() external view returns (uint64);
        function roots(uint64 period) external view returns (
            bytes32 inputsHash, bytes32 payoutRoot, bytes32 billRoot, uint256 totalCumPayout, uint256 totalCumBill,
            uint64 registryBlock, uint32 priceTableVersion, uint64 publishedAt, bool voided
        );
        function digest(RootInput r) external view returns (bytes32);
        function publishRoot(RootInput r, bytes[] sigs) external;
        function totalDeposited() external view returns (uint256);
        event AccountCreated(bytes32 indexed id, address key);
    }
}

/// Every account the Settlement contract created up to `block`, as it stood at `block`: the balances a
/// period may bill and the keys its configs must be signed with.
pub async fn chain_accounts(p: &DynProvider, settlement_addr: Address, block: u64) -> Result<Vec<AccountState>, Error> {
    let filter = Filter::new()
        .address(settlement_addr)
        .event_signature(ISettlement::AccountCreated::SIGNATURE_HASH)
        .from_block(0)
        .to_block(block);
    let mut ids: Vec<B256> = p
        .get_logs(&filter)
        .await?
        .iter()
        .filter_map(|l| l.log_decode::<ISettlement::AccountCreated>().ok())
        .map(|d| d.inner.data.id)
        .collect();
    ids.sort();
    ids.dedup();
    let mut out = Vec::new();
    for id in ids {
        let label: Vec<u8> = id.iter().copied().take_while(|b| *b != 0).collect();
        let id = AccountId::new(String::from_utf8_lossy(&label));
        out.push(settlement::account(p, settlement_addr, &id, Some(block)).await?);
    }
    Ok(out)
}

/// The on-chain daily price of each distinct tier at `block`.
pub async fn tier_prices(
    p: &DynProvider,
    settlement_addr: Address,
    version: u32,
    tiers: impl IntoIterator<Item = Tier>,
    block: u64,
) -> Result<Vec<TierPrice>, Error> {
    let mut out: Vec<TierPrice> = Vec::new();
    for tier in tiers {
        if out.iter().any(|x| x.tier == tier) {
            continue;
        }
        let per_day = settlement::price(p, settlement_addr, version, &tier, Some(block)).await?;
        out.push(TierPrice { tier, per_day });
    }
    Ok(out)
}

/// A published root as the contract holds it: `(inputsHash, payoutRoot, billRoot, voided)`.
pub async fn root_of(p: &DynProvider, settlement_addr: Address, period: u64) -> Result<(B256, B256, B256, bool), Error> {
    let r = ISettlement::new(settlement_addr, p.clone()).roots(period).call().await?;
    Ok((r.inputsHash, r.payoutRoot, r.billRoot, r.voided))
}

/// Every decoded event of the registry and the cells contract from the first block to `to_block`, in chain
/// order: the period's identity input. ponytail: one `eth_getLogs` over the whole history; chunk from a
/// checkpoint when it grows.
pub async fn registry_events(
    p: &DynProvider,
    contracts: &[Address],
    to_block: u64,
) -> Result<Vec<RegistryEvent>, Error> {
    let filter = Filter::new()
        .address(contracts.to_vec())
        .from_block(0)
        .to_block(to_block);
    let logs = p.get_logs(&filter).await?;
    Ok(logs.iter().filter_map(Identity::decode).collect())
}

/// The latest period on the contract and whether it was voided.
pub async fn last_period(p: &DynProvider, settlement: Address) -> Result<(u64, bool), Error> {
    let c = ISettlement::new(settlement, p.clone());
    let last = c.lastPeriod().call().await?;
    if last == 0 {
        return Ok((0, false));
    }
    let r = c.roots(last).call().await?;
    Ok((last, r.voided))
}

pub fn root_input(r: &Roots) -> ISettlement::RootInput {
    ISettlement::RootInput {
        period: r.period,
        inputsHash: r.inputs_hash,
        registryBlock: r.registry_block,
        priceTableVersion: r.price_table_version,
        payoutRoot: r.payout_root,
        billRoot: r.bill_root,
        totalCumPayout: alloy::primitives::U256::from(r.total_cum_payout),
        totalCumBill: alloy::primitives::U256::from(r.total_cum_bill),
    }
}

/// The contract's own digest of the roots, to check the local encoding before anyone signs.
pub async fn contract_digest(p: &DynProvider, settlement: Address, r: &Roots) -> Result<B256, Error> {
    Ok(ISettlement::new(settlement, p.clone())
        .digest(root_input(r))
        .call()
        .await?)
}

/// `publishRoot` with the gathered signatures; the transaction hash.
pub async fn publish(
    p: &DynProvider,
    settlement: Address,
    r: &Roots,
    sigs: Vec<Bytes>,
) -> Result<B256, Error> {
    let receipt = ISettlement::new(settlement, p.clone())
        .publishRoot(root_input(r), sigs)
        .send()
        .await?
        .get_receipt()
        .await?;
    if !receipt.status() {
        return Err("publishRoot reverted".into());
    }
    Ok(receipt.transaction_hash)
}
