//! The auditor's transactions: World ID registration of its two roles on the registry, `attest` batches to the
//! cells contract.

use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use alloy::providers::DynProvider;
use alloy::sol;
use dedan_core::config::Deployment;
use dedan_core::types::*;
use tracing::info;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

sol! {
    #[sol(rpc)]
    interface IDedanRegistry {
        struct WorldIdProof {
            uint256 nullifier;
            uint256 nonce;
            uint64 expiresAtMin;
            uint64 issuerSchemaId;
            uint256 credentialGenesisIssuedAtMin;
            uint256[5] proof;
        }
        function isAttester(address account) external view returns (bool);
        function isAuditor(address account) external view returns (bool);
        function minStake(uint8 kind) external view returns (uint256);
        function registerAttester(WorldIdProof p) external;
        function registerAuditor(WorldIdProof p, uint256 stake) external;
    }

    #[sol(rpc)]
    interface IDedanCells {
        struct Sample {
            bytes32 labelHash;
            uint32 rttMs;
            uint32 hitTtfbMs;
            bytes token;
            bytes prefix;
        }
        function attest(uint64 cell, Sample[] batch) external;
    }

    #[sol(rpc)]
    interface IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
    }
}

/// The devnet's World ID mock accepts any proof with a non-zero nullifier and burns it per action, so one
/// derived from the account and the action stands in for the real proof.
fn proof(me: Address, action: &str) -> IDedanRegistry::WorldIdProof {
    let n = keccak256([me.as_slice(), action.as_bytes()].concat());
    IDedanRegistry::WorldIdProof {
        nullifier: U256::from_be_bytes(n.0),
        nonce: U256::ZERO,
        expiresAtMin: 0,
        issuerSchemaId: 1, // Proof of Human: the only credential the registry takes
        credentialGenesisIssuedAtMin: U256::ZERO,
        proof: [U256::ZERO; 5],
    }
}

/// Grants this human its roles if it does not hold them yet: `ROLE_ATTESTER` for a proof alone,
/// `ROLE_AUDITOR` for a proof and the governance minimum stake.
pub async fn register(p: &DynProvider, dep: &Deployment, me: Address) -> Result<(), Error> {
    let reg = IDedanRegistry::new(dep.dedan_registry, p.clone());
    if !reg.isAttester(me).call().await? {
        reg.registerAttester(proof(me, "dedan-attester"))
            .send()
            .await?
            .get_receipt()
            .await?;
        info!("registered as attester with a World ID proof");
    }
    if !reg.isAuditor(me).call().await? {
        let stake = reg.minStake(1).call().await?; // StakeKind.Auditor
        IERC20::new(dep.usdc, p.clone())
            .approve(dep.dedan_registry, stake)
            .send()
            .await?
            .get_receipt()
            .await?;
        reg.registerAuditor(proof(me, "dedan-auditor"), stake)
            .send()
            .await?
            .get_receipt()
            .await?;
        info!(stake = %stake, "registered as auditor with a World ID proof and the minimum stake");
    }
    Ok(())
}

pub fn row(a: &Attestation) -> IDedanCells::Sample {
    IDedanCells::Sample {
        labelHash: a.node_id.label_hash(),
        rttMs: a.rtt_ms,
        hitTtfbMs: a.hit_ttfb_ms.unwrap_or(0),
        token: Bytes::copy_from_slice(a.token.as_bytes()),
        prefix: Bytes::copy_from_slice(a.prefix.as_bytes()),
    }
}

/// One `attest(cell, batch)` transaction on the cells contract; returns its hash.
pub async fn attest(p: &DynProvider, cells: Address, cell: Cell, batch: &[Attestation]) -> Result<B256, Error> {
    let receipt = IDedanCells::new(cells, p.clone())
        .attest(cell.0, batch.iter().map(row).collect())
        .send()
        .await?
        .get_receipt()
        .await?;
    if !receipt.status() {
        return Err("attest transaction reverted".into());
    }
    Ok(receipt.transaction_hash)
}
