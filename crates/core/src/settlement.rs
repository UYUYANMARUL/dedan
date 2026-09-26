//! What every service reads from the Settlement contract (ARCHITECTURE.md §8, §9): an account's key and
//! balances, the price of a tier, and the checks built on them. A config counts only with the signature of
//! the account key registered on chain, so the control plane relays configs but cannot write one.

use crate::assign::AccountInfo;
use crate::envelope::Envelope;
use crate::types::*;
use alloy::primitives::{keccak256, Address, B256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolValue;
use serde::{Deserialize, Serialize};

sol! {
    #[sol(rpc)]
    interface ISettlementReads {
        function accounts(bytes32 id) external view returns (address key, uint256 balance, uint256 appliedBill, uint256 pendingWithdrawal, uint64 withdrawableAt);
        function prices(uint32 version, bytes32 tier) external view returns (uint256);
    }
}

pub type SettlementError = Box<dyn std::error::Error + Send + Sync>;

/// An account as the contract holds it, micro-USDC.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountState {
    pub id: AccountId,
    pub key: Address,
    pub balance: u128,
    pub applied_bill: u128,
    pub pending_withdrawal: u128,
}

impl AccountState {
    /// What the account can still be billed (§8): its balance minus the bills published but not yet applied
    /// (`cum_bill` is its latest cumulative bill) and minus a withdrawal on its way out.
    pub fn effective_balance(&self, cum_bill: u128) -> u128 {
        self.balance
            .saturating_sub(cum_bill.saturating_sub(self.applied_bill))
            .saturating_sub(self.pending_withdrawal)
    }
}

fn u128_of(v: alloy::primitives::U256) -> u128 {
    u128::try_from(v).unwrap_or(u128::MAX)
}

/// `accounts(id)` at `block` (the head when `None`); a zero key means the account does not exist.
pub async fn account<P: Provider + Clone>(
    p: &P,
    settlement: Address,
    id: &AccountId,
    block: Option<u64>,
) -> Result<AccountState, SettlementError> {
    let c = ISettlementReads::new(settlement, p.clone());
    let mut call = c.accounts(id.bytes32());
    if let Some(b) = block {
        call = call.block(b.into());
    }
    let r = call.call().await?;
    Ok(AccountState {
        id: id.clone(),
        key: r.key,
        balance: u128_of(r.balance),
        applied_bill: u128_of(r.appliedBill),
        pending_withdrawal: u128_of(r.pendingWithdrawal),
    })
}

/// The price table's key for a tier: `keccak256(abi.encode(uint16 nodes, uint32 classMbps, uint32 availabilityBps))`.
pub fn tier_key(t: &Tier) -> B256 {
    keccak256((u16::from(t.nodes), t.class.mbps(), t.availability.bps()).abi_encode())
}

/// A tier's list price per day at `block`, micro-USDC; zero when the table has no price for it.
pub async fn price<P: Provider + Clone>(
    p: &P,
    settlement: Address,
    version: u32,
    tier: &Tier,
    block: Option<u64>,
) -> Result<u128, SettlementError> {
    let c = ISettlementReads::new(settlement, p.clone());
    let mut call = c.prices(version, tier_key(tier));
    if let Some(b) = block {
        call = call.block(b.into());
    }
    Ok(u128_of(call.call().await?))
}

/// The share of a daily price one period of `period_secs` bills, micro-USDC.
pub fn list_price(per_day: u128, period_secs: u64) -> u128 {
    per_day * period_secs as u128 / 86_400
}

/// A config counts only when the account key registered on chain signed it as the account (§9).
pub fn verify_config(env: &Envelope<AccountConfig>, key: Address) -> Result<(), String> {
    let id = &env.payload.account_id;
    if key == Address::ZERO {
        return Err(format!("{id} has no key on chain"));
    }
    if env.signer != id.signer() {
        return Err(format!("signed as {}, not as {}", env.signer, id.signer()));
    }
    match env.recover() {
        Ok(a) if a == key => Ok(()),
        Ok(a) => Err(format!("signed by {a}; the key of {id} on chain is {key}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Keeps the newest config per account: a lower version is a replay and is refused; the same version again
/// (a full sync) changes nothing.
pub fn newer(current: Option<u64>, incoming: u64) -> bool {
    current.is_none_or(|c| incoming > c)
}

impl AccountInfo {
    /// The assignment's view of a verified config: the tier and the customer's node policy.
    pub fn from_config(c: &AccountConfig) -> Self {
        Self {
            id: c.account_id.clone(),
            nodes: c.tier.nodes,
            class: c.tier.class,
            availability: c.tier.availability,
            min_node_stake: c.node_policy.min_node_stake,
            operator_allowlist: c.node_policy.operator_allowlist.clone(),
            operator_denylist: c.node_policy.operator_denylist.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::signers::local::PrivateKeySigner;

    fn cfg(version: u64) -> AccountConfig {
        AccountConfig {
            account_id: AccountId::new("acme"),
            account_key: Address::ZERO,
            domains: vec!["www.images.com".into()],
            origins: vec![Origin {
                url: "http://origin".into(),
                host_header: None,
                weight: 1,
            }],
            tier: Tier {
                nodes: 6,
                class: Class::M100,
                availability: Availability::P999,
            },
            cache_rules: CacheRules {
                default_ttl_s: 60,
                keep_query_string: true,
                max_object_bytes: 1 << 20,
            },
            asset_list: "http://origin/assets/manifest.txt".into(),
            node_policy: NodePolicy::default(),
            version,
            seq: 0,
        }
    }

    #[test]
    fn only_the_account_key_signs_a_config() {
        let customer = PrivateKeySigner::from_slice(&[1u8; 32]).unwrap();
        let cp = PrivateKeySigner::from_slice(&[2u8; 32]).unwrap();
        let good = Envelope::sign(cfg(1), "account:acme", 10, 1, &customer).unwrap();
        assert_eq!(verify_config(&good, customer.address()), Ok(()));
        assert!(verify_config(&good, Address::ZERO).is_err(), "no account on chain");
        let by_cp = Envelope::sign(cfg(1), "cp", 10, 1, &cp).unwrap();
        assert!(verify_config(&by_cp, customer.address()).is_err(), "the control plane cannot write a config");
        let forged = Envelope::sign(cfg(1), "account:acme", 10, 1, &cp).unwrap();
        assert!(verify_config(&forged, customer.address()).is_err());
        let mut tampered = good.clone();
        tampered.payload.origins[0].url = "http://evil".into();
        assert!(verify_config(&tampered, customer.address()).is_err());
    }

    #[test]
    fn versions_only_move_forward() {
        assert!(newer(None, 1));
        assert!(newer(Some(1), 2));
        assert!(!newer(Some(2), 2), "the same version again is a no-op");
        assert!(!newer(Some(2), 1), "an older config is a replay");
    }

    #[test]
    fn tier_keys_and_period_prices() {
        // keccak256(abi.encode(uint16(6), uint32(100), uint32(9990))), as `Settlement.tierKey` computes it
        let t = cfg(1).tier;
        let mut enc = [0u8; 96];
        enc[31] = 6;
        enc[62..64].copy_from_slice(&100u16.to_be_bytes());
        enc[94..96].copy_from_slice(&9990u16.to_be_bytes());
        assert_eq!(tier_key(&t), keccak256(enc));
        assert_eq!(list_price(7_200_000_000, 120), 10_000_000);
        assert_eq!(list_price(7_200_000_000, 86_400), 7_200_000_000);
        let s = AccountState {
            balance: 100,
            applied_bill: 30,
            pending_withdrawal: 20,
            ..Default::default()
        };
        assert_eq!(s.effective_balance(50), 60, "20 billed but unapplied, 20 leaving");
        assert_eq!(s.effective_balance(500), 0);
    }
}
