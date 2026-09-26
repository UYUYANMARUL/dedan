//! OpenZeppelin `StandardMerkleTree` encoding, the one `MerkleProof.verify` checks on-chain:
//! leaf = `keccak256(keccak256(abi.encode(...)))`, pairs hashed in sorted order, leaves sorted ascending
//! and laid out as `@openzeppelin/merkle-tree` does, so roots and proofs match that library byte for byte.
//! `contracts/test/Merkle.sol` is the Solidity twin.

use alloy::primitives::{keccak256, Address, B256, U256};

/// Double-hashed leaf over already ABI-encoded bytes.
pub fn leaf(abi_encoded: &[u8]) -> B256 {
    keccak256(keccak256(abi_encoded))
}

/// `(address payee, uint256 cumulativePayout)` as the Settlement contract's `claim` verifies it.
pub fn payout_leaf(payee: Address, cumulative: u128) -> B256 {
    let mut b = [0u8; 64];
    b[12..32].copy_from_slice(payee.as_slice());
    b[32..].copy_from_slice(&U256::from(cumulative).to_be_bytes::<32>());
    leaf(&b)
}

/// `(bytes32 accountId, uint256 cumulativeBill)` as `executeWithdraw` verifies it.
pub fn bill_leaf(account: B256, cumulative: u128) -> B256 {
    let mut b = [0u8; 64];
    b[..32].copy_from_slice(account.as_slice());
    b[32..].copy_from_slice(&U256::from(cumulative).to_be_bytes::<32>());
    leaf(&b)
}

/// `keccak256(min ‖ max)`.
pub fn pair(a: B256, b: B256) -> B256 {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    keccak256([lo.as_slice(), hi.as_slice()].concat())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    nodes: Vec<B256>,
    leaves: usize,
}

impl Tree {
    pub fn new(mut leaves: Vec<B256>) -> Self {
        leaves.sort();
        let n = leaves.len();
        if n == 0 {
            return Self {
                nodes: vec![],
                leaves: 0,
            };
        }
        let len = 2 * n - 1;
        let mut nodes = vec![B256::ZERO; len];
        for (i, l) in leaves.iter().enumerate() {
            nodes[len - 1 - i] = *l;
        }
        for i in (0..len - n).rev() {
            nodes[i] = pair(nodes[2 * i + 1], nodes[2 * i + 2]);
        }
        Self { nodes, leaves: n }
    }

    /// Zero for an empty tree.
    pub fn root(&self) -> B256 {
        self.nodes.first().copied().unwrap_or(B256::ZERO)
    }

    pub fn proof(&self, leaf: B256) -> Option<Vec<B256>> {
        let len = self.nodes.len();
        let mut j = (len - self.leaves..len).find(|&j| self.nodes[j] == leaf)?;
        let mut proof = Vec::new();
        while j > 0 {
            let sibling = if j % 2 == 1 { j + 1 } else { j - 1 };
            proof.push(self.nodes[sibling]);
            j = (j - 1) / 2;
        }
        Some(proof)
    }
}

pub fn verify(proof: &[B256], root: B256, leaf: B256) -> bool {
    proof.iter().fold(leaf, |acc, p| pair(acc, *p)) == root
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AccountId;

    // Computed with Foundry's cast (see PLAN.md M1): the smoke's payout leaves and one more.
    const LEAF_A: &str = "0xebb138cc587bf79f60418f8fbcf1324bbd1f4c0c1035140d072b1f046cd20030"; // node op, 80 USDC
    const LEAF_B: &str = "0x4392f80ed8f0da4fb5c42763ff5e3d5fbc5feec10d64b34d61dedec4371cd3d7"; // treasury, 20 USDC
    const LEAF_C: &str = "0xfbeb5854e19558ba2a07381793700b99276cbdf96b8edfb5024bbb2ce5b3a863"; // index 3, 5 USDC
    const ROOT_2: &str = "0xcdbf393e7e748746515db892ae390bbd9c2870351fff85a0e1f0f5f3c2985be6";
    const ROOT_3: &str = "0x6e316b952785dfc64967dd32238e19d56ecb34de86853fbd7fe8a61eb287e31e";
    const BILL_ACME_100: &str =
        "0x1b7fbf901f0d039e1a6c60cbae3cb6acd281d4d3b8f8c1e938aa84d6cde4d123";

    fn h(s: &str) -> B256 {
        s.parse().unwrap()
    }

    #[test]
    fn leaves_match_the_contract_encoding() {
        let node_op: Address = "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"
            .parse()
            .unwrap();
        let gov: Address = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
            .parse()
            .unwrap();
        assert_eq!(payout_leaf(node_op, 80_000_000), h(LEAF_A));
        assert_eq!(payout_leaf(gov, 20_000_000), h(LEAF_B));
        assert_eq!(
            bill_leaf(AccountId::new("acme").bytes32(), 100_000_000),
            h(BILL_ACME_100)
        );
    }

    #[test]
    fn roots_and_proofs_match_cast() {
        let two = Tree::new(vec![h(LEAF_A), h(LEAF_B)]);
        assert_eq!(two.root(), h(ROOT_2));
        assert_eq!(two.proof(h(LEAF_A)).unwrap(), vec![h(LEAF_B)]);

        let three = Tree::new(vec![h(LEAF_A), h(LEAF_B), h(LEAF_C)]);
        assert_eq!(three.root(), h(ROOT_3));
        for l in [LEAF_A, LEAF_B, LEAF_C] {
            let proof = three.proof(h(l)).unwrap();
            assert!(verify(&proof, three.root(), h(l)), "proof for {l}");
            assert!(!verify(&proof, three.root(), h(BILL_ACME_100)));
        }
        assert!(three.proof(h(BILL_ACME_100)).is_none());
    }

    #[test]
    fn degenerate_trees() {
        assert_eq!(Tree::new(vec![]).root(), B256::ZERO);
        let one = Tree::new(vec![h(LEAF_A)]);
        assert_eq!(one.root(), h(LEAF_A));
        assert_eq!(one.proof(h(LEAF_A)).unwrap(), Vec::<B256>::new());
        assert!(verify(&[], h(LEAF_A), h(LEAF_A)));
    }
}
