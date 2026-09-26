//! Signed envelopes (ARCHITECTURE.md §11): `{payload, signer, ts, nonce, sig}`.
//!
//! `sig` is a secp256k1 signature in the EIP-191 form over
//! `keccak256(signer ‖ 0x00 ‖ ts ‖ nonce ‖ canonical JSON of the payload)`, so one signer implementation
//! serves envelopes and the settlement roots the contracts verify. Canonical JSON = keys sorted, no
//! whitespace, which is what `serde_json::Value` produces. A node may have several signer keys (the holders
//! of `ROLE_NODE_SIGNER` on its name), so the verifier asks whether the recovered address is one of them.

use alloy::primitives::{keccak256, Address, Signature, B256};
use alloy::signers::{local::PrivateKeySigner, SignerSync};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Envelopes whose timestamp is further than this from the verifier's clock are rejected.
pub const MAX_SKEW_S: u64 = 300;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub payload: T,
    /// `node:<label>`, `resolver:<address>`, `auditor:<address>`, `account:<label>`, `operator:<address>` or `cp`.
    pub signer: String,
    pub ts: u64,
    pub nonce: u64,
    /// 65-byte `r ‖ s ‖ v` signature, 0x-hex.
    pub sig: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("malformed signature")]
    BadSignature,
    #[error("{0} is not signed by a key registered for it")]
    WrongSigner(String),
    #[error("timestamp outside the {MAX_SKEW_S} s window")]
    Skew,
    #[error("nonce already seen")]
    Replay,
    #[error("payload is not serialisable: {0}")]
    Json(String),
}

/// Where a verifier checks a recovered address against a signer string: the identity cache in production, a
/// plain map on the devnet.
pub trait KeyLookup {
    fn accepts(&self, signer: &str, address: Address) -> bool;
}

impl KeyLookup for HashMap<String, Address> {
    fn accepts(&self, signer: &str, address: Address) -> bool {
        self.get(signer) == Some(&address)
    }
}

pub fn canonical_json<T: Serialize>(payload: &T) -> Result<Vec<u8>, EnvelopeError> {
    let value = serde_json::to_value(payload).map_err(|e| EnvelopeError::Json(e.to_string()))?;
    serde_json::to_vec(&value).map_err(|e| EnvelopeError::Json(e.to_string()))
}

/// `keccak256(signer ‖ 0x00 ‖ ts ‖ nonce ‖ payload)`, before the EIP-191 prefix.
pub fn message_hash(signer: &str, ts: u64, nonce: u64, payload_json: &[u8]) -> B256 {
    let mut buf = Vec::with_capacity(signer.len() + 17 + payload_json.len());
    buf.extend_from_slice(signer.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&ts.to_be_bytes());
    buf.extend_from_slice(&nonce.to_be_bytes());
    buf.extend_from_slice(payload_json);
    keccak256(&buf)
}

impl<T: Serialize> Envelope<T> {
    pub fn sign(
        payload: T,
        signer: impl Into<String>,
        ts: u64,
        nonce: u64,
        key: &PrivateKeySigner,
    ) -> Result<Self, EnvelopeError> {
        let signer = signer.into();
        let hash = message_hash(&signer, ts, nonce, &canonical_json(&payload)?);
        let sig = key
            .sign_message_sync(hash.as_slice())
            .map_err(|_| EnvelopeError::BadSignature)?;
        Ok(Self {
            payload,
            signer,
            ts,
            nonce,
            sig: format!("0x{}", hex::encode(sig.as_bytes())),
        })
    }

    pub fn hash(&self) -> Result<B256, EnvelopeError> {
        Ok(message_hash(
            &self.signer,
            self.ts,
            self.nonce,
            &canonical_json(&self.payload)?,
        ))
    }

    /// The address that produced `sig` for this exact content.
    pub fn recover(&self) -> Result<Address, EnvelopeError> {
        let raw = hex::decode(self.sig.trim_start_matches("0x"))
            .map_err(|_| EnvelopeError::BadSignature)?;
        let sig = Signature::try_from(raw.as_slice()).map_err(|_| EnvelopeError::BadSignature)?;
        sig.recover_address_from_msg(self.hash()?.as_slice())
            .map_err(|_| EnvelopeError::BadSignature)
    }

    /// Full check: clock window, a registered key of the signer, replay.
    pub fn verify(
        &self,
        keys: &impl KeyLookup,
        now: u64,
        nonces: &mut NonceCache,
    ) -> Result<(), EnvelopeError> {
        if now.abs_diff(self.ts) > MAX_SKEW_S {
            return Err(EnvelopeError::Skew);
        }
        if !keys.accepts(&self.signer, self.recover()?) {
            return Err(EnvelopeError::WrongSigner(self.signer.clone()));
        }
        if !nonces.insert(&self.signer, self.nonce, self.ts, now) {
            return Err(EnvelopeError::Replay);
        }
        Ok(())
    }
}

/// Nonces seen per signer inside the skew window. Anything older fails the timestamp check anyway, so the
/// cache never needs to remember more than one window.
#[derive(Debug, Default)]
pub struct NonceCache {
    seen: HashMap<(String, u64), u64>,
}

impl NonceCache {
    /// Records the nonce; `false` when it was already seen.
    pub fn insert(&mut self, signer: &str, nonce: u64, ts: u64, now: u64) -> bool {
        // ponytail: full scan on every insert; a per-signer ring buffer if a verifier sees thousands per second
        self.seen
            .retain(|_, seen_ts| now.saturating_sub(*seen_ts) <= MAX_SKEW_S);
        self.seen.insert((signer.to_owned(), nonce), ts).is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Ping {
        n: u32,
        s: String,
    }

    fn key() -> PrivateKeySigner {
        PrivateKeySigner::from_slice(&[7u8; 32]).unwrap()
    }

    fn keys(k: &PrivateKeySigner) -> HashMap<String, Address> {
        HashMap::from([("node:frankfurt".to_string(), k.address())])
    }

    fn ping() -> Ping {
        Ping {
            n: 3,
            s: "hello".into(),
        }
    }

    #[test]
    fn round_trip_through_json() {
        let k = key();
        let env = Envelope::sign(ping(), "node:frankfurt", 1_000, 1, &k).unwrap();
        let json = serde_json::to_string(&env).unwrap();
        let back: Envelope<Ping> = serde_json::from_str(&json).unwrap();
        let mut nonces = NonceCache::default();
        assert_eq!(back.verify(&keys(&k), 1_010, &mut nonces), Ok(()));
        assert_eq!(back.payload, ping());
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let k = key();
        let mut env = Envelope::sign(ping(), "node:frankfurt", 1_000, 1, &k).unwrap();
        env.payload.n += 1;
        let mut nonces = NonceCache::default();
        assert_eq!(
            env.verify(&keys(&k), 1_000, &mut nonces),
            Err(EnvelopeError::WrongSigner("node:frankfurt".into()))
        );
    }

    #[test]
    fn replay_skew_and_unknown_signer_are_rejected() {
        let k = key();
        let env = Envelope::sign(ping(), "node:frankfurt", 1_000, 42, &k).unwrap();
        let mut nonces = NonceCache::default();
        assert_eq!(env.verify(&keys(&k), 1_000, &mut nonces), Ok(()));
        assert_eq!(
            env.verify(&keys(&k), 1_000, &mut nonces),
            Err(EnvelopeError::Replay)
        );
        assert_eq!(
            env.verify(&keys(&k), 1_000 + MAX_SKEW_S + 1, &mut nonces),
            Err(EnvelopeError::Skew)
        );
        assert_eq!(
            env.verify(&HashMap::new(), 1_000, &mut nonces),
            Err(EnvelopeError::WrongSigner("node:frankfurt".into()))
        );
    }

    /// What the aggregator does: parse the envelope's JSON text, then recover the signer from the re-canonicalised
    /// payload. Every field of a probe result must survive that round trip byte for byte.
    #[test]
    fn probe_batch_envelope_survives_the_wire() {
        use crate::types::*;
        let k = key();
        let batch = ProbeBatch {
            auditor_id: AuditorId(k.address()),
            period: 7,
            results: vec![
                ProbeResult {
                    probe: ProbeType::Targeted,
                    account: AccountId::new("acme"),
                    asset: "/images/photo-1.png".into(),
                    target: Some(NodeId::new("seoul")),
                    node_id: Some(NodeId::new("seoul")),
                    ok: true,
                    failure: None,
                    metrics: ProbeMetrics {
                        ttfb_ms: Some(586),
                        hit_ttfb_ms: Some(251),
                        rtt_ms: Some(250),
                        kbps: Some(1_234_567),
                        bytes: Some(21_733),
                        sha256: Some("ab".repeat(32)),
                        status: Some(200),
                    },
                    token: Some(format!("seoul.29840419.{}", "0".repeat(64))),
                    prefix: "10.77.16.0/24".into(),
                    ts: 1_790_425_100,
                },
                ProbeResult {
                    probe: ProbeType::Landing,
                    account: AccountId::new("acme"),
                    asset: "/".into(),
                    target: None,
                    node_id: None,
                    ok: false,
                    failure: Some(FailureClass::Connect),
                    metrics: ProbeMetrics::default(),
                    token: None,
                    prefix: "10.77.16.0/24".into(),
                    ts: 1_790_425_101,
                },
            ],
        };
        let env = Envelope::sign(batch, format!("auditor:{}", k.address()), 1_790_425_110, 3, &k).unwrap();
        let text = serde_json::to_string(&env).unwrap();
        let back: Envelope<ProbeBatch> = serde_json::from_str(&text).unwrap();
        assert_eq!(back.recover().unwrap(), k.address());
        assert_eq!(back.hash().unwrap(), env.hash().unwrap());
    }

    #[test]
    fn canonical_json_sorts_keys_and_strips_whitespace() {
        let v = serde_json::json!({"b": 1, "a": {"d": [1, 2], "c": null}});
        assert_eq!(
            canonical_json(&v).unwrap(),
            br#"{"a":{"c":null,"d":[1,2]},"b":1}"#
        );
    }
}
