//! The control-plane client (ARCHITECTURE.md §5, §9): signed account configs by full sync and over the event
//! stream, purges (verified against the account key inside the config), and the certificate orders.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dedan_core::envelope::Envelope;
use dedan_core::types::*;
use tokio_stream::StreamExt;
use tracing::{info, warn};

use crate::{unix_now, Node, Route};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

impl Node {
    fn cp(&self) -> Option<&str> {
        self.cp_url.as_deref()
    }

    /// A config is taken only with the control plane's signature, then its hostnames route to its origin.
    fn apply_config(&self, env: Envelope<AccountConfig>) {
        match env.recover() {
            Ok(a) if env.signer == "cp" && a == self.dep.control_plane => {}
            _ => {
                warn!(account = %env.payload.account_id, "config not signed by the control plane; ignored");
                return;
            }
        }
        let c = env.payload;
        let Some(origin) = c.origins.first() else { return };
        let origin_url = self.origin_override.clone().unwrap_or_else(|| origin.url.clone());
        let Ok((addr, tls, host)) = crate::parse_origin(&origin_url) else { return };
        let mut routes = self.routes.write().expect("routes");
        routes.retain(|_, r| r.account != c.account_id.0);
        for h in &c.domains {
            routes.insert(
                h.to_lowercase(),
                Route {
                    account: c.account_id.0.clone(),
                    origin_addr: addr.clone(),
                    origin_tls: tls,
                    origin_host: origin.host_header.clone().unwrap_or_else(|| host.clone()),
                    generation: 0,
                },
            );
        }
        self.account_keys
            .write()
            .expect("keys")
            .insert(c.account_id.0.clone(), c.account_key);
        info!(account = %c.account_id, version = c.version, seq = c.seq, domains = ?c.domains, "config applied");
    }

    /// A purge signed by the account key bumps the account's cache generation: every key changes at once.
    /// ponytail: url and prefix scopes bump the whole account too; `Storage::purge` per key is the next step.
    fn apply_purge(&self, env: Envelope<Purge>) {
        let account = env.payload.account_id.0.clone();
        let key = self.account_keys.read().expect("keys").get(&account).copied();
        match (env.recover(), key) {
            (Ok(a), Some(k)) if env.signer == env.payload.account_id.signer() && a == k => {}
            _ => {
                warn!(account, "purge not signed by the account key; ignored");
                return;
            }
        }
        let mut gens = self.generations.write().expect("generations");
        let g = gens.entry(account.clone()).or_default();
        *g += 1;
        info!(account, generation = *g, scope = ?env.payload.scope, pattern = env.payload.pattern, "purged");
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
            self.apply_config(serde_json::from_value(c)?);
            n += 1;
        }
        Ok(n)
    }

    /// The event stream: `config` and `purge` events as they happen; reconnects after a pause.
    pub async fn stream_events(self: Arc<Self>) {
        let Some(cp) = self.cp().map(str::to_owned) else { return };
        loop {
            // the client's 5 s total timeout would cut the stream every 5 s; the read timeout still catches a dead peer
            match self.http.get(format!("{cp}/v1/stream")).timeout(Duration::from_secs(86_400)).send().await {
                Ok(resp) => {
                    let mut body = resp.bytes_stream();
                    let mut buf = String::new();
                    let mut event = String::new();
                    while let Some(chunk) = body.next().await {
                        let Ok(chunk) = chunk else { break };
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                        while let Some(pos) = buf.find('\n') {
                            let line = buf[..pos].trim_end_matches('\r').to_owned();
                            buf.drain(..=pos);
                            if let Some(e) = line.strip_prefix("event:") {
                                event = e.trim().to_owned();
                            } else if let Some(d) = line.strip_prefix("data:") {
                                let data = d.trim();
                                match event.as_str() {
                                    "config" => match serde_json::from_str(data) {
                                        Ok(env) => self.apply_config(env),
                                        Err(e) => warn!(error = %e, "bad config event"),
                                    },
                                    "purge" => match serde_json::from_str(data) {
                                        Ok(env) => self.apply_purge(env),
                                        Err(e) => warn!(error = %e, "bad purge event"),
                                    },
                                    _ => {}
                                }
                            } else if line.is_empty() {
                                event.clear();
                            }
                        }
                    }
                    // ponytail: events sent while disconnected are lost (a missed purge lasts until the TTL); replay by
                    // Last-Event-ID from the cp's seq if a cp restart ever matters
                    warn!("control plane stream ended; reconnecting");
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
            let req = CertRequest {
                node_id: self.label.clone(),
                account_id: AccountId::new(account.clone()),
                hostnames: sans.clone(),
                csr_pem,
            };
            let env = match Envelope::sign(
                req,
                self.label.signer(),
                unix_now(),
                self.nonce.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
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
                Ok(issued) => match self.certs.load(&sans, &issued.chain_pem, &key_der) {
                    Ok(()) => info!(sans = ?sans, "certificate installed"),
                    Err(e) => warn!(error = %e, "certificate not loadable"),
                },
                Err(e) => warn!(error = %e, "certificate response"),
            }
        }
    }
}
