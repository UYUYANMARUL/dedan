//! World ID 4.0 at the moment Dedan grants a role (WORLD-ID.md). The control plane is the relying party:
//!
//!   GET  /v1/world-id/config    whether World ID is configured, the app, the environment, the roles
//!   POST /v1/world-id/request   {role}: the RP signature IDKit needs (a fresh nonce, five minutes, the action)
//!   POST /v1/world-id/verify    {role, account, result}: the IDKit result checked here (World ID 4.0, the action,
//!                               the environment, a nonce this control plane signed and nobody used, the Proof of
//!                               Human credential, the account as the signal), verified at the Developer Portal,
//!                               the nullifier not yet burned on chain; then the proof the registry takes: attested
//!                               for `WorldIdGate` with the registrar key, or the IDKit proof itself when the
//!                               registry points at the real `WorldIDVerifier` (World Chain)

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use alloy::sol;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use dedan_core::worldid::{action_field, hash_signal, sign_request};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use super::{unix_now, Cp};

sol! {
    #[sol(rpc)]
    interface IRegistryWorldId {
        function nullifierUsed(uint256 action, uint256 nullifier) external view returns (bool);
        function worldIdRpId() external view returns (uint64);
        function worldId() external view returns (address);
        function signalHash(address account) external view returns (uint256);
        function isAttester(address account) external view returns (bool);
        function isAuditor(address account) external view returns (bool);
        function isOperator(address account) external view returns (bool);
    }
    #[sol(rpc)]
    interface IWorldIdGate {
        function attestationDigest(
            address registry, uint256 nullifier, uint256 action, uint64 rpId, uint256 nonce, uint256 signalHash,
            uint64 expiresAtMin, uint64 issuerSchemaId, uint256 credentialGenesisIssuedAtMin, uint256 deadline
        ) external view returns (bytes32);
    }
}

/// The roles a World ID grants, each its own action, so its own nullifiers.
pub const ROLES: [(&str, &str); 3] = [
    ("attester", "dedan-attester"),
    ("auditor", "dedan-auditor"),
    ("operator", "dedan-operator"),
];

/// How long a registrar attestation stays valid on chain.
const ATTESTATION_TTL: u64 = 3600;
/// How long an RP signature (and so an IDKit request) stays valid.
const REQUEST_TTL: u64 = 300;

pub fn action_of(role: &str) -> Option<&'static str> {
    ROLES.iter().find(|(r, _)| *r == role).map(|(_, a)| *a)
}

/// The app registered in the Developer Portal, from the environment (`deploy/devnet/world-id.env` on the devnet).
pub struct WorldIdCfg {
    pub app_id: String,
    pub rp_id: String,
    pub signing_key: PrivateKeySigner,
    /// `staging` (the simulator) or `production` (World App).
    pub environment: String,
    pub verify_url: String,
}

impl WorldIdCfg {
    pub fn from_env() -> Option<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let (app_id, rp_id, key) = (
            get("DEDAN_WORLD_ID_APP_ID")?,
            get("DEDAN_WORLD_ID_RP_ID")?,
            get("DEDAN_WORLD_ID_SIGNING_KEY")?,
        );
        let signing_key = match key.trim().parse() {
            Ok(k) => k,
            Err(e) => {
                warn!(error = %e, "DEDAN_WORLD_ID_SIGNING_KEY is not a secp256k1 key: World ID disabled");
                return None;
            }
        };
        let environment = get("DEDAN_WORLD_ID_ENV").unwrap_or_else(|| "staging".into());
        let verify_url = get("DEDAN_WORLD_ID_VERIFY_URL").unwrap_or_else(|| {
            if environment == "production" {
                "https://developer.world.org".into()
            } else {
                "https://staging-developer.worldcoin.org".into()
            }
        });
        Some(Self {
            app_id,
            rp_id,
            signing_key,
            environment,
            verify_url: verify_url.trim_end_matches('/').to_owned(),
        })
    }
}

/// The relying party's state: its app, and the request nonces it signed and nobody has answered yet.
pub struct WorldId {
    pub cfg: Option<WorldIdCfg>,
    issued: std::sync::Mutex<HashMap<U256, (&'static str, u64)>>,
}

impl WorldId {
    pub fn from_env() -> Self {
        let cfg = WorldIdCfg::from_env();
        match &cfg {
            Some(c) => info!(app_id = c.app_id, rp_id = c.rp_id, environment = c.environment, "World ID relying party"),
            None => info!("World ID not configured (DEDAN_WORLD_ID_APP_ID, _RP_ID, _SIGNING_KEY): roles only through the devnet mock"),
        }
        Self {
            cfg,
            issued: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

/// What the registry's `WorldIdProof` needs from a checked IDKit result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub nonce: U256,
    pub nullifier: U256,
    pub expires_at_min: u64,
    pub proof: [U256; 5],
}

fn field(v: &Value, name: &str) -> Result<U256, String> {
    let s = v[name].as_str().ok_or_else(|| format!("{name} is missing"))?;
    U256::from_str(s).map_err(|e| format!("{name}: {e}"))
}

/// The checks the Developer Portal does not make for us: World ID 4.0, this role's action, this environment, the
/// Proof of Human credential and nothing weaker, and the proof bound to the account that will hold the role.
pub fn check_result(result: &Value, action: &str, environment: &str, account: Address) -> Result<Checked, String> {
    if result["protocol_version"].as_str() != Some("4.0") {
        return Err("only World ID 4.0 proofs are accepted".into());
    }
    if result["action"].as_str() != Some(action) {
        return Err(format!("the proof is for action {}, not {action}", result["action"]));
    }
    if result["environment"].as_str() != Some(environment) {
        return Err(format!("the proof is from environment {}, not {environment}", result["environment"]));
    }
    let item = result["responses"]
        .as_array()
        .and_then(|r| {
            r.iter()
                .find(|i| i["identifier"] == "proof_of_human" && i["issuer_schema_id"].as_u64() == Some(1))
        })
        .ok_or("the proof is not Proof of Human: a role needs a unique human (Orb)")?;
    let signal = field(item, "signal_hash").map_err(|_| "the proof carries no signal: it must name the account".to_string())?;
    if signal != U256::from_be_bytes(hash_signal(&account.to_string()).0) {
        return Err(format!("the proof names another account than {account}"));
    }
    let words = item["proof"].as_array().filter(|p| p.len() == 5).ok_or("the proof must have five words")?;
    let mut proof = [U256::ZERO; 5];
    for (i, w) in words.iter().enumerate() {
        proof[i] = U256::from_str(w.as_str().unwrap_or("")).map_err(|e| format!("proof word {i}: {e}"))?;
    }
    Ok(Checked {
        nonce: field(result, "nonce")?,
        nullifier: field(item, "nullifier")?,
        expires_at_min: item["expires_at_min"].as_u64().ok_or("expires_at_min is missing")?,
        proof,
    })
}

type ApiError = (StatusCode, Json<Value>);

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    (code, Json(json!({ "error": msg.into() })))
}

fn configured(s: &Cp) -> Result<&WorldIdCfg, ApiError> {
    s.world_id.cfg.as_ref().ok_or_else(|| {
        err(
            StatusCode::SERVICE_UNAVAILABLE,
            "World ID is not configured: set DEDAN_WORLD_ID_APP_ID, DEDAN_WORLD_ID_RP_ID and DEDAN_WORLD_ID_SIGNING_KEY (WORLD-ID.md)",
        )
    })
}

pub async fn config(State(s): State<Arc<Cp>>) -> Json<Value> {
    let roles: serde_json::Map<String, Value> = ROLES.iter().map(|(r, a)| (r.to_string(), json!(a))).collect();
    Json(match &s.world_id.cfg {
        Some(c) => json!({
            "configured": true, "app_id": c.app_id, "rp_id": c.rp_id, "environment": c.environment,
            "credential": "proof_of_human", "roles": roles,
        }),
        None => json!({ "configured": false, "roles": roles }),
    })
}

#[derive(Deserialize)]
pub struct RequestBody {
    role: String,
}

pub async fn request(State(s): State<Arc<Cp>>, Json(body): Json<RequestBody>) -> Result<Json<Value>, ApiError> {
    let cfg = configured(&s)?;
    let action = action_of(&body.role).ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unknown role {}", body.role)))?;
    let now = unix_now();
    // 32 bytes from the OS generator (a fresh key's bytes; the nonce is their field hash)
    let random = PrivateKeySigner::random().to_bytes().0;
    let sig = sign_request(&cfg.signing_key, Some(action), random, now, REQUEST_TTL)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    {
        let mut issued = s.world_id.issued.lock().expect("issued");
        issued.retain(|_, (_, exp)| *exp >= now);
        issued.insert(U256::from_be_bytes(sig.nonce.0), (action, sig.expires_at));
    }
    Ok(Json(json!({
        "app_id": cfg.app_id, "action": action, "environment": cfg.environment,
        "action_description": format!("Dedan: become a {} (one per human)", body.role),
        "rp_context": {
            "rp_id": cfg.rp_id, "nonce": sig.nonce, "created_at": sig.created_at, "expires_at": sig.expires_at,
            "signature": sig.sig,
        },
    })))
}

#[derive(Deserialize)]
pub struct VerifyBody {
    role: String,
    account: Address,
    result: Value,
}

pub async fn verify(State(s): State<Arc<Cp>>, Json(body): Json<VerifyBody>) -> Result<Json<Value>, ApiError> {
    let cfg = configured(&s)?;
    let action = action_of(&body.role).ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unknown role {}", body.role)))?;
    let bad = |m: String| err(StatusCode::BAD_REQUEST, m);
    let checked = check_result(&body.result, action, &cfg.environment, body.account).map_err(bad)?;
    // the request must be one this control plane signed for this action, answered once
    {
        let mut issued = s.world_id.issued.lock().expect("issued");
        match issued.remove(&checked.nonce) {
            Some((a, _)) if a == action => {}
            Some(_) => return Err(err(StatusCode::BAD_REQUEST, "the request was signed for another role")),
            None => return Err(err(StatusCode::BAD_REQUEST, "unknown, used or expired request: start again")),
        }
    }

    // the zero-knowledge proof, checked by World
    let url = format!("{}/api/v4/verify/{}", cfg.verify_url, cfg.rp_id);
    let resp = s
        .http
        .post(&url)
        .json(&body.result)
        .send()
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, format!("Developer Portal unreachable: {e}")))?;
    let status = resp.status();
    let portal: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() || portal["success"] != json!(true) {
        warn!(%status, portal = %portal, "Developer Portal rejected the proof");
        let why = portal["detail"]
            .as_str()
            .or(portal["message"].as_str())
            .or(portal["code"].as_str())
            .unwrap_or("rejected");
        return Err(err(StatusCode::BAD_REQUEST, format!("the Developer Portal rejected the proof: {why}")));
    }
    if portal["environment"].as_str() != Some(cfg.environment.as_str()) {
        return Err(bad(format!("the Developer Portal verified it for {}", portal["environment"])));
    }
    if let Some(n) = portal["nullifier"].as_str() {
        if U256::from_str(n).ok() != Some(checked.nullifier) {
            return Err(bad("the Developer Portal answered another nullifier".into()));
        }
    }

    // one human, one key for this role: the registry burns the nullifier; refuse before anyone pays gas
    let registry = IRegistryWorldId::new(s.dep.dedan_registry, s.provider.clone());
    let chain = |e: alloy::contract::Error| err(StatusCode::BAD_GATEWAY, format!("chain: {e}"));
    let action_f = action_field(action);
    if registry.nullifierUsed(action_f, checked.nullifier).call().await.map_err(chain)? {
        return Err(err(StatusCode::CONFLICT, format!("this human already holds the {} role with another key: one per human", body.role)));
    }
    let holds = match body.role.as_str() {
        "attester" => registry.isAttester(body.account).call().await,
        "auditor" => registry.isAuditor(body.account).call().await,
        _ => registry.isOperator(body.account).call().await,
    }
    .map_err(chain)?;
    if holds {
        return Err(err(StatusCode::CONFLICT, format!("{} already holds the {} role", body.account, body.role)));
    }

    // the proof the registry takes: attested for the gate, or World's own proof for the real verifier
    let rp_id = registry.worldIdRpId().call().await.map_err(chain)?;
    let verifier = registry.worldId().call().await.map_err(chain)?;
    let signal = registry.signalHash(body.account).call().await.map_err(chain)?;
    let head = s.provider.get_block_number().await.map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let now = dedan_core::identity::block_time(&s.provider, head)
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let deadline = U256::from(now + ATTESTATION_TTL);
    let gate = IWorldIdGate::new(verifier, s.provider.clone());
    let (mode, words) = match gate
        .attestationDigest(
            s.dep.dedan_registry,
            checked.nullifier,
            action_f,
            rp_id,
            checked.nonce,
            signal,
            checked.expires_at_min,
            1,
            U256::ZERO,
            deadline,
        )
        .call()
        .await
    {
        Ok(digest) => {
            let sig = s
                .key
                .sign_message_sync(digest.as_slice())
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let b = sig.as_bytes();
            let (r, sv, v) = (U256::from_be_slice(&b[..32]), U256::from_be_slice(&b[32..64]), U256::from(b[64]));
            ("attested", [r, sv, v, deadline, U256::ZERO])
        }
        // the registry points at World's own verifier: it checks the zero-knowledge proof itself
        Err(_) => ("onchain", checked.proof),
    };
    info!(role = body.role, account = %body.account, nullifier = %checked.nullifier, mode, "World ID verified at the Developer Portal");
    let dec = |x: U256| x.to_string();
    Ok(Json(json!({
        "role": body.role, "action": action, "account": body.account, "mode": mode,
        "nullifier": format!("0x{:064x}", checked.nullifier),
        "proof": {
            "nullifier": dec(checked.nullifier), "nonce": dec(checked.nonce), "expires_at_min": checked.expires_at_min,
            "issuer_schema_id": 1, "credential_genesis_issued_at_min": 0, "proof": words.map(dec),
        },
        "portal": portal,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(account: &str) -> Value {
        json!({
            "protocol_version": "4.0", "nonce": "0x008ae1aa597fa146ebd3aa2ceddf360668dea5e526567e92b0321816a4e895bd",
            "action": "dedan-attester", "environment": "staging",
            "responses": [{
                "identifier": "proof_of_human", "issuer_schema_id": 1, "expires_at_min": 1_800_000_000u64,
                "signal_hash": hash_signal(account).to_string(), "nullifier": "0x04e5f6",
                "proof": ["0x1", "0x2", "0x3", "0x4", "0x5"],
            }],
        })
    }

    #[test]
    fn only_a_proof_of_human_for_this_account_action_and_environment_passes() {
        let a: Address = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8".parse().unwrap();
        let ok = check_result(&result(&a.to_string()), "dedan-attester", "staging", a).unwrap();
        assert_eq!(ok.nullifier, U256::from(0x04e5f6));
        assert_eq!(ok.proof[4], U256::from(5));
        assert!(check_result(&result(&a.to_string()), "dedan-auditor", "staging", a).is_err(), "another role's action");
        assert!(check_result(&result(&a.to_string()), "dedan-attester", "production", a).is_err(), "another environment");
        let other: Address = "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC".parse().unwrap();
        assert!(check_result(&result(&other.to_string()), "dedan-attester", "staging", a).is_err(), "another account");
        let mut selfie = result(&a.to_string());
        selfie["responses"][0]["identifier"] = json!("selfie");
        selfie["responses"][0]["issuer_schema_id"] = json!(11);
        assert!(check_result(&selfie, "dedan-attester", "staging", a).unwrap_err().contains("Proof of Human"));
        let mut v3 = result(&a.to_string());
        v3["protocol_version"] = json!("3.0");
        assert!(check_result(&v3, "dedan-attester", "staging", a).is_err(), "legacy proofs keep another nullifier space");
        let mut unsigned = result(&a.to_string());
        unsigned["responses"][0].as_object_mut().unwrap().remove("signal_hash");
        assert!(check_result(&unsigned, "dedan-attester", "staging", a).is_err(), "a proof must name the account");
    }

    #[test]
    fn roles_map_to_actions() {
        assert_eq!(action_of("attester"), Some("dedan-attester"));
        assert_eq!(action_of("operator"), Some("dedan-operator"));
        assert_eq!(action_of("root"), None);
    }
}
