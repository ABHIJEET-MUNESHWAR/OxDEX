//! Jito block-engine bundle client (stubbed).
//!
//! In production this would speak the Jito gRPC protocol via the
//! [`jito-searcher-client`](https://github.com/jito-labs/searcher-examples)
//! crate. To keep this workspace dependency-light and offline-buildable,
//! we ship two implementations:
//!
//!  * [`HttpJitoClient`] — thin wrapper around the public JSON-RPC
//!    `sendBundle` method, suitable for testnets.
//!  * [`InMemoryJitoClient`] — captures bundles in-process; used by tests.
//!
//! Both implement the [`BundleSubmitter`] trait so the rest of the system
//! can be swapped without changing call sites.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, instrument};

use oxdex_types::Solution;

/// Errors produced by the bundle layer.
#[derive(Debug, Error)]
pub enum BundleError {
    /// Network / HTTP failure.
    #[error("transport error: {0}")]
    Transport(String),
    /// Server returned an error.
    #[error("server error: {0}")]
    Server(String),
}

/// Bundle of base64-encoded Solana transactions plus an optional tip.
#[derive(Debug, Clone, Serialize)]
pub struct Bundle {
    /// Base64 transactions in execution order.
    pub transactions: Vec<String>,
    /// Tip in lamports — informational only; the tip tx must be inside `transactions`.
    pub tip_lamports: u64,
    /// Free-form id for tracing; set by the caller.
    pub trace_id: String,
}

/// Pluggable bundle submitter.
#[async_trait]
pub trait BundleSubmitter: Send + Sync + 'static {
    /// Submit a bundle, returning a server-assigned id.
    async fn submit(&self, bundle: Bundle) -> Result<String, BundleError>;
}

// ---------- HTTP client ----------

/// JSON-RPC `sendBundle` client.
pub struct HttpJitoClient {
    url: String,
    http: reqwest::Client,
    tip_lamports: u64,
}

impl HttpJitoClient {
    /// Construct against `block_engine_url`.
    pub fn new(block_engine_url: impl Into<String>, tip_lamports: u64) -> Self {
        Self {
            url: block_engine_url.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            tip_lamports,
        }
    }

    /// Default tip configured on this client.
    pub fn tip_lamports(&self) -> u64 {
        self.tip_lamports
    }
}

#[async_trait]
impl BundleSubmitter for HttpJitoClient {
    #[instrument(skip(self, bundle), fields(trace_id = %bundle.trace_id, n_tx = bundle.transactions.len()))]
    async fn submit(&self, bundle: Bundle) -> Result<String, BundleError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": bundle.trace_id,
            "method": "sendBundle",
            "params": [ bundle.transactions ],
        });
        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| BundleError::Transport(e.to_string()))?;
        let status = resp.status();
        let txt = resp
            .text()
            .await
            .map_err(|e| BundleError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(BundleError::Server(format!("{status}: {txt}")));
        }
        let v: serde_json::Value = serde_json::from_str(&txt)
            .map_err(|e| BundleError::Server(format!("decode: {e} ({txt})")))?;
        if let Some(err) = v.get("error") {
            return Err(BundleError::Server(err.to_string()));
        }
        Ok(v.get("result")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown")
            .to_string())
    }
}

// ---------- in-memory test client ----------

/// In-memory submitter — keeps every bundle for assertion in tests.
#[derive(Default, Clone)]
pub struct InMemoryJitoClient {
    inner: Arc<Mutex<Vec<Bundle>>>,
}

impl InMemoryJitoClient {
    /// New empty client.
    pub fn new() -> Self {
        Self::default()
    }
    /// Snapshot of all submitted bundles so far.
    pub fn submitted(&self) -> Vec<Bundle> {
        self.inner.lock().clone()
    }
}

#[async_trait]
impl BundleSubmitter for InMemoryJitoClient {
    async fn submit(&self, bundle: Bundle) -> Result<String, BundleError> {
        let id = format!("inmem-{}", self.inner.lock().len());
        self.inner.lock().push(bundle);
        Ok(id)
    }
}

/// Encode a [`Solution`] as a placeholder transaction (real implementation
/// would build a Solana versioned tx invoking the OxDEX settlement program).
pub fn encode_solution_as_placeholder_tx(solution: &Solution) -> String {
    use base64::Engine;
    let bytes = serde_json::to_vec(solution).unwrap_or_default();
    debug!(bytes = bytes.len(), "encoding placeholder tx for solution");
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_submitter_records_bundle() {
        let c = InMemoryJitoClient::new();
        let id = c
            .submit(Bundle {
                transactions: vec!["AAA".into()],
                tip_lamports: 1000,
                trace_id: "t1".into(),
            })
            .await
            .unwrap();
        assert_eq!(id, "inmem-0");
        assert_eq!(c.submitted().len(), 1);
    }
}
