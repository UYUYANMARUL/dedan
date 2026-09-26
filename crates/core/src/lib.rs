//! `dedan-core`: the shared, pure part of Dedan (ARCHITECTURE.md v3).
//!
//! Everything that money or steering depends on lives here, so that resolvers, auditors, the aggregator,
//! the countersigner and `recompute` run the same code: signed envelopes, fetch tokens, the Merkle
//! encoding the contracts verify, the latency map, the slot assignment, the scoring function, the identity
//! cache built from the Dedan registry's events, the node records read from ENS, the probe schedule and the
//! configuration files every service starts from. Nothing in this crate
//! does I/O except `identity::Identity::replay` and `ens::EnsClient`.

pub mod assign;
pub mod config;
pub mod ens;
pub mod envelope;
pub mod geo;
pub mod identity;
pub mod map;
pub mod merkle;
pub mod schedule;
pub mod score;
pub mod token;
pub mod types;

pub use alloy::primitives::{Address, B256, U256};
