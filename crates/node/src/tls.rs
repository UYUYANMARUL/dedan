//! Certificates (ARCHITECTURE.md §10): the node makes a key and a CSR per SAN set, the control plane runs the
//! ACME order, the chain comes back and is served for its names from an in-memory store the TLS listener
//! consults per SNI; renewal at two thirds of the lifetime. Private keys never leave the node.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

use crate::unix_now;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
struct Held {
    key: Arc<CertifiedKey>,
    not_before: u64,
    not_after: u64,
}

/// What the listener serves: one certified key per hostname, swapped without a restart.
#[derive(Default)]
pub struct CertStore {
    by_name: RwLock<HashMap<String, Held>>,
}

impl fmt::Debug for CertStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CertStore({} names)", self.by_name.read().map(|m| m.len()).unwrap_or(0))
    }
}

impl CertStore {
    /// True when `name` has no certificate or is past two thirds of its lifetime.
    pub fn needs(&self, name: &str) -> bool {
        let now = unix_now();
        match self.by_name.read().expect("certs").get(&name.to_lowercase()) {
            None => true,
            Some(h) => now >= h.not_before + (h.not_after.saturating_sub(h.not_before)) * 2 / 3,
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.by_name.read().expect("certs").keys().cloned().collect()
    }

    /// Loads a PEM chain with its PKCS#8 key for every SAN it covers.
    pub fn load(&self, sans: &[String], chain_pem: &str, key_der: &[u8]) -> Result<(), Error> {
        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut chain_pem.as_bytes())
            .collect::<Result<_, _>>()?;
        let leaf = certs.first().ok_or("empty certificate chain")?;
        let (_, parsed) = x509_parser::parse_x509_certificate(leaf)?;
        let not_before = parsed.validity().not_before.timestamp().max(0) as u64;
        let not_after = parsed.validity().not_after.timestamp().max(0) as u64;
        let key = PrivateKeyDer::Pkcs8(key_der.to_vec().into());
        let signing = rustls::crypto::ring::sign::any_supported_type(&key)?;
        let held = Held {
            key: Arc::new(CertifiedKey::new(certs, signing)),
            not_before,
            not_after,
        };
        let mut map = self.by_name.write().expect("certs");
        for s in sans {
            map.insert(s.to_lowercase(), held.clone());
        }
        Ok(())
    }
}

impl ResolvesServerCert for CertStore {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let name = hello.server_name()?.to_lowercase();
        self.by_name.read().ok()?.get(&name).map(|h| h.key.clone())
    }
}

/// A fresh key pair and a CSR for `sans` (the first one is the subject).
pub fn csr(sans: &[String]) -> Result<(String, Vec<u8>), Error> {
    let key = rcgen::KeyPair::generate()?;
    let params = rcgen::CertificateParams::new(sans.to_vec())?;
    let req = params.serialize_request(&key)?;
    Ok((req.pem()?, key.serialize_der()))
}
