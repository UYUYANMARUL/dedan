//! World ID 4.0 (IDKit) as Dedan's backend speaks it (WORLD-ID.md): the field hash IDKit applies to actions and
//! signals, and the relying party's request signature (docs.world.org/world-id/idkit/signatures), which IDKit
//! only ships for JavaScript and Go; this port is checked against the published test vector.

use alloy::primitives::{keccak256, B256, U256};
use alloy::signers::{local::PrivateKeySigner, SignerSync};
use serde::{Deserialize, Serialize};

/// `keccak256(input) >> 8`: a 32-byte value that fits the proof system's field.
pub fn hash_to_field(input: &[u8]) -> B256 {
    let h = keccak256(input);
    let mut out = [0u8; 32];
    out[1..].copy_from_slice(&h[..31]);
    B256::from(out)
}

/// IDKit's `hashSignal`: a `0x`-prefixed even-length hex string (an address) is hashed as its bytes, anything
/// else as UTF-8. The registry's `signalHash(account)` is the same value for an account.
pub fn hash_signal(signal: &str) -> B256 {
    match signal.strip_prefix("0x") {
        Some(h) if !h.is_empty() && h.len() % 2 == 0 && h.bytes().all(|b| b.is_ascii_hexdigit()) => {
            hash_to_field(&hex::decode(h).expect("checked hex"))
        }
        _ => hash_to_field(signal.as_bytes()),
    }
}

/// An action as the proof carries it (the registry's `ACTION_*` constants).
pub fn action_field(action: &str) -> U256 {
    U256::from_be_bytes(hash_to_field(action.as_bytes()).0)
}

/// `rp_<16 hex>` from the Developer Portal as the `uint64` the on-chain verifier takes.
pub fn rp_id_u64(rp_id: &str) -> Option<u64> {
    u64::from_str_radix(rp_id.strip_prefix("rp_")?, 16).ok()
}

/// What the backend hands the browser for `IDKit.request({ rp_context })`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpSignature {
    /// `r ‖ s ‖ v` (v = 27 or 28), 0x-hex.
    pub sig: String,
    pub nonce: B256,
    pub created_at: u64,
    pub expires_at: u64,
}

/// `0x01 ‖ nonce ‖ created_at (u64 BE) ‖ expires_at (u64 BE) ‖ hash_to_field(action)?`: 49 or 81 bytes.
pub fn rp_message(nonce: B256, created_at: u64, expires_at: u64, action: Option<&str>) -> Vec<u8> {
    let mut m = Vec::with_capacity(81);
    m.push(0x01);
    m.extend_from_slice(nonce.as_slice());
    m.extend_from_slice(&created_at.to_be_bytes());
    m.extend_from_slice(&expires_at.to_be_bytes());
    if let Some(a) = action {
        m.extend_from_slice(hash_to_field(a.as_bytes()).as_slice());
    }
    m
}

/// The RP signature over a fresh nonce (`hash_to_field(random)`), valid `ttl` seconds from `created_at`, signed
/// EIP-191 with the RP signing key.
pub fn sign_request(
    key: &PrivateKeySigner,
    action: Option<&str>,
    random: [u8; 32],
    created_at: u64,
    ttl: u64,
) -> Result<RpSignature, alloy::signers::Error> {
    let nonce = hash_to_field(&random);
    let expires_at = created_at + ttl;
    let sig = key.sign_message_sync(&rp_message(nonce, created_at, expires_at, action))?;
    Ok(RpSignature {
        sig: format!("0x{}", hex::encode(sig.as_bytes())),
        nonce,
        created_at,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// docs.world.org/world-id/idkit/signatures: hash_to_field("hello")
    #[test]
    fn hash_to_field_vector() {
        assert_eq!(
            hash_to_field(b"hello").to_string(),
            "0x001c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36dea"
        );
    }

    /// The published sign_request vector: key 0xabab…, random bytes 0x00..0x1f, created 1700000000, TTL 300,
    /// action "test-action".
    #[test]
    fn sign_request_matches_the_published_vector() {
        let key: PrivateKeySigner = "0xabababababababababababababababababababababababababababababababab".parse().unwrap();
        let mut random = [0u8; 32];
        for (i, b) in random.iter_mut().enumerate() {
            *b = i as u8;
        }
        let s = sign_request(&key, Some("test-action"), random, 1_700_000_000, 300).unwrap();
        assert_eq!(s.nonce.to_string(), "0x008ae1aa597fa146ebd3aa2ceddf360668dea5e526567e92b0321816a4e895bd");
        assert_eq!(
            hex::encode(rp_message(s.nonce, s.created_at, s.expires_at, Some("test-action"))),
            "01008ae1aa597fa146ebd3aa2ceddf360668dea5e526567e92b0321816a4e895bd000000006553f100000000006553f22c00aa0ce59768ae5b1c52f07a9387f14f09f277422c0d2f8a268c7bad0c60a46a"
        );
        assert_eq!(
            s.sig,
            "0x05594adb6c1495768a38d523d7d6ee6356b2c31231919198794ed022ade7d08f73753f83bd167067d99c9b969d28e9222315837c66af25867b041273a6d5056f1b"
        );
    }

    #[test]
    fn signals_actions_and_rp_ids() {
        // an address is hashed as its 20 bytes, as the registry's `signalHash(account)` does
        let a = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
        let bytes = hex::decode(&a[2..]).unwrap();
        assert_eq!(hash_signal(a), hash_to_field(&bytes));
        assert_eq!(hash_signal("user-123"), hash_to_field(b"user-123"));
        // the registry's ACTION_ATTESTER = uint256(keccak256("dedan-attester")) >> 8
        assert_eq!(action_field("dedan-attester"), U256::from_be_bytes(keccak256(b"dedan-attester").0) >> 8);
        assert_eq!(rp_id_u64("rp_123456789abcdef0"), Some(0x1234_5678_9abc_def0));
        assert_eq!(rp_id_u64("app_x"), None);
    }
}
