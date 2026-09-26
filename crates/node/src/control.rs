//! The control-plane client (ARCHITECTURE.md §5, §9): account configs by full sync and over the event stream,
//! each verified against the account key registered on chain and applied only when newer; purges verified
//! against the same key; after a reconnect the stream replays what came after the last event seen, or says
//! `reset` when that is no longer possible; and the certificate orders.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::Address;
use dedan_core::envelope::Envelope;
use dedan_core::settlement::{self, newer, verify_config};
use dedan_core::types::*;

use tokio_stream::StreamExt;
use tracing::{info, warn};

use crate::{unix_now, Node};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

impl Node {
    fn cp(&self) -> Option<&str> {
        self.cp_url.as_deref()
    }

    /// `check` against the account key on chain: the cached key first, then the chain once more before refusing
    /// (an account created or a key rotated since).
    async fn verify_account(
        &self,
        account: &AccountId,
        check: impl Fn(Address) -> Result<(), String>,
    ) -> Result<(), String> {
        let cached = self.account_keys.read().expect("keys").get(&account.0).copied();
        if cached.is_some_and(|k| check(k).is_ok()) {
            return Ok(());
        }
        let key = settlement::account(&self.provider, self.dep.settlement, account, None)
            .await
            .map_err(|e| format!("account key not readable on chain: {e}"))?
            .key;
        self.account_keys.write().expect("keys").insert(account.0.clone(), key);
        check(key)
    }

    /// A config counts only when the account key on chain signed it, and only a newer version replaces the one
    /// applied: the same version again (every full sync) changes nothing, a lower one is a replay.
    async fn apply_config(&self, env: Envelope<AccountConfig>) {
        let c = &env.payload;
        let current = self.versions.read().expect("versions").get(&c.account_id.0).copied();
        if !newer(current, c.version) {
            if current != Some(c.version) {
                warn!(account = %c.account_id, version = c.version, ?current, "older config refused (a replay?)");
            }
            return;
        }
        if let Err(e) = self.verify_account(&c.account_id, |k| verify_config(&env, k)).await {
            warn!(account = %c.account_id, version = c.version, error = %e, "config refused");
            return;
        }
        let routes = match crate::routes_of(c, self.origin_override.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                warn!(account = %c.account_id, error = %e, "config refused: origin not usable");
                return;
            }
        };
        // checked again under the lock: the stream and the full sync may apply at the same time
        let mut versions = self.versions.write().expect("versions");
        if !newer(versions.get(&c.account_id.0).copied(), c.version) {
            return;
        }
        versions.insert(c.account_id.0.clone(), c.version);
        let mut table = self.routes.write().expect("routes");
        table.retain(|_, r| r.account != c.account_id.0);
        table.extend(routes);
        info!(account = %c.account_id, version = c.version, signer = env.signer, domains = ?c.domains, rules = ?c.cache_rules, "config applied");
    }

    /// A purge signed by the account key on chain bumps the account's cache generation: every key changes at once.
    /// ponytail: url and prefix scopes bump the whole account too; `Storage::purge` per key is the next step.
    async fn apply_purge(&self, env: Envelope<Purge>) {
        let account = env.payload.account_id.clone();
        let signed = |k: Address| match env.recover() {
            Ok(a) if env.signer == account.signer() && a == k && k != Address::ZERO => Ok(()),
            _ => Err("not signed by the account key".to_owned()),
        };
        if let Err(e) = self.verify_account(&account, signed).await {
            warn!(%account, error = %e, "purge refused");
            return;
        }
        let g = self.bump(&account.0);
        info!(%account, generation = g, scope = ?env.payload.scope, pattern = env.payload.pattern, "purged");
    }

    fn bump(&self, account: &str) -> u64 {
        let mut gens = self.generations.write().expect("generations");
        let g = gens.entry(account.to_owned()).or_default();
        *g += 1;
        *g
    }

    /// Events were missed (the control plane's log no longer reaches back to the last one seen, or its history
    /// changed): every config again, and every account's objects dropped, since a purge may be among them.
    async fn resync(&self) {
        if let Err(e) = self.full_sync().await {
            warn!(error = %e, "full sync after a reset failed");
        }
        let accounts: HashSet<String> = self.routes.read().expect("routes").values().map(|r| r.account.clone()).collect();
        for a in &accounts {
            self.bump(a);
        }
        warn!(accounts = ?accounts, "control plane reset: configs synced, every account's cache dropped");
    }

    pub async fn full_sync(&self) -> Result<usize, Error> {
        let Some(cp) = self.cp() else { return Ok(0) };
        let v: serde_json::Value = self
            .http
            .get(format!("{cp}/v1/configs"))
            .send()
            .await?
            .json()
            .await?;
        let mut n = 0;
        for c in v["configs"].as_array().cloned().unwrap_or_default() {
            self.apply_config(serde_json::from_value(c)?).await;
            n += 1;
        }
        // before the stream's first event: it replays from this sync on, so nothing sent in between is missed
        let seq = v["seq"].as_u64().unwrap_or(0);
        let _ = self.last_seq.compare_exchange(0, seq, Ordering::Relaxed, Ordering::Relaxed);
        Ok(n)
    }

    async fn dispatch(&self, event: &str, data: &str) {
        match event {
            "config" => match serde_json::from_str(data) {
                Ok(env) => self.apply_config(env).await,
                Err(e) => warn!(error = %e, "bad config event"),
            },
            "purge" => match serde_json::from_str(data) {
                Ok(env) => self.apply_purge(env).await,
                Err(e) => warn!(error = %e, "bad purge event"),
            },
            "reset" => self.resync().await,
            _ => {} // the keep-alive comments
        }
    }

    /// The event stream: `config` and `purge` events as they happen; reconnects after a pause with the last
    /// event seen (`Last-Event-ID`), so the control plane replays what was sent meanwhile.
    pub async fn stream_events(self: Arc<Self>) {
        let Some(cp) = self.cp().map(str::to_owned) else { return };
        loop {
            // the client's 5 s total timeout would cut the stream every 5 s; the read timeout still catches a dead peer
            let mut req = self.http.get(format!("{cp}/v1/stream")).timeout(Duration::from_secs(86_400));
            let last = self.last_seq.load(Ordering::Relaxed);
            if last > 0 {
                req = req.header("last-event-id", last.to_string());
            }
            match req.send().await {
                Ok(resp) => {
                    let mut body = resp.bytes_stream();
                    let (mut buf, mut event, mut data, mut id) = (String::new(), String::new(), String::new(), None);
                    while let Some(chunk) = body.next().await {
                        let Ok(chunk) = chunk else { break };
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                        while let Some(pos) = buf.find('\n') {
                            let line = buf[..pos].trim_end_matches('\r').to_owned();
                            buf.drain(..=pos);
                            if line.is_empty() {
                                self.dispatch(&event, &data).await;
                                if let Some(seq) = id.take() {
                                    self.last_seq.store(seq, Ordering::Relaxed);
                                }
                                event.clear();
                                data.clear();
                            } else if let Some(v) = line.strip_prefix("event:") {
                                event = v.trim().to_owned();
                            } else if let Some(v) = line.strip_prefix("data:") {
                                data.push_str(v.trim());
                            } else if let Some(v) = line.strip_prefix("id:") {
                                id = v.trim().parse::<u64>().ok();
                            }
                        }
                    }
                    warn!(last_seq = self.last_seq.load(Ordering::Relaxed), "control plane stream ended; reconnecting");
                }
                Err(e) => warn!(error = %e, "control plane stream unreachable"),
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    /// One CSR per account (its hostnames plus this node's own name) whenever a name needs a certificate.
    pub async fn renew_certificates(&self) {
        let Some(cp) = self.cp() else { return };
        let own = format!("{}.node.{}", self.label, self.zone);
        let mut sets: HashMap<String, Vec<String>> = HashMap::new();
        for (host, r) in self.routes.read().expect("routes").iter() {
            sets.entry(r.account.clone()).or_default().push(host.clone());
        }
        if sets.is_empty() {
            sets.insert(String::new(), vec![]);
        }
        for (account, mut sans) in sets {
            sans.sort();
            sans.push(own.clone());
            if !sans.iter().any(|s| self.certs.needs(s)) {
                continue;
            }
            let (csr_pem, key_der) = match crate::tls::csr(&sans) {
                Ok(x) => x,
                Err(e) => {
                    warn!(error = %e, "csr");
                    continue;
                }
            };
            let order = CertRequest {
                node_id: self.label.clone(),
                account_id: AccountId::new(account.clone()),
                hostnames: sans.clone(),
                csr_pem,
                // ARI: the certificate this one renews, if any
                replaces: sans.iter().find_map(|s| self.certs.ari_id(s)),
            };
            let env = match Envelope::sign(
                order,
                self.label.signer(),
                unix_now(),
                self.nonce.fetch_add(1, Ordering::Relaxed),
                &self.key,
            ) {
                Ok(e) => e,
                Err(e) => {
                    warn!(error = %e, "cert request envelope");
                    continue;
                }
            };
            let resp = match self.http.post(format!("{cp}/v1/certs")).json(&env).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "certificate request not delivered");
                    continue;
                }
            };
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!(%status, body, sans = ?sans, "certificate refused");
                continue;
            }
            match resp.json::<CertIssued>().await {
                Ok(issued) => match self.certs.load(&sans, &issued.chain_pem, &key_der, issued.ari_id) {
                    Ok(()) => info!(sans = ?sans, "certificate installed"),
                    Err(e) => warn!(error = %e, "certificate not loadable"),
                },
                Err(e) => warn!(error = %e, "certificate response"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::signers::local::PrivateKeySigner;

    fn config(version: u64, origin: &str) -> AccountConfig {
        let file = serde_json::json!({
            "label": "acme", "hostnames": ["www.images.com"], "nodes": 6, "class": "100M", "availability": "99.9",
            "origin": origin, "cache_rules": {"default_ttl_s": 60, "keep_query_string": false, "max_object_bytes": 1024},
        });
        serde_json::from_value::<dedan_core::config::AccountCfg>(file).unwrap().to_config(version)
    }

    fn origin_of(node: &Node) -> String {
        node.routes.read().unwrap()["www.images.com"].origin_addr.clone()
    }

    #[tokio::test]
    async fn configs_need_the_account_key_and_only_move_forward() {
        let customer = PrivateKeySigner::from_slice(&[1u8; 32]).unwrap();
        let cp = PrivateKeySigner::from_slice(&[2u8; 32]).unwrap();
        let node = crate::tests::node();
        // the key as the chain holds it; the test node's RPC is unreachable, so nothing else can be read
        node.account_keys.write().unwrap().insert("acme".into(), customer.address());
        let signed = |v: u64, origin: &str, key: &PrivateKeySigner, signer: &str| {
            Envelope::sign(config(v, origin), signer, 1, v, key).unwrap()
        };
        node.apply_config(signed(5, "http://by-cp", &cp, "cp")).await;
        assert!(node.routes.read().unwrap().is_empty(), "the control plane cannot write a config");
        node.apply_config(signed(5, "http://origin", &customer, "account:acme")).await;
        assert_eq!(origin_of(&node), "origin:80");
        assert_eq!(node.routes.read().unwrap()["www.images.com"].rules.default_ttl_s, 60);
        node.apply_config(signed(4, "http://old", &customer, "account:acme")).await;
        node.apply_config(signed(5, "http://same-version", &customer, "account:acme")).await;
        assert_eq!(origin_of(&node), "origin:80", "a lower or equal version changes nothing");
        node.apply_config(signed(6, "http://origin-2:8080", &customer, "account:acme")).await;
        assert_eq!(origin_of(&node), "origin-2:8080");
    }
}
