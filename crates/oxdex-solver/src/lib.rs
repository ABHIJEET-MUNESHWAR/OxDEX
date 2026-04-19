//! Solver abstraction.
//!
//! A [`Solver`] takes a [`Batch`] and returns one [`Solution`]. We model
//! it as an `async` trait so future solvers can call out to off-chain
//! services (Jupiter quote API, RFQ providers, …) without blocking the
//! auctioneer's tokio reactor.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use async_trait::async_trait;
use std::time::Duration;

use oxdex_matching::Matcher;
use oxdex_types::{Address, Batch, OxDexError, Result, Solution};

/// Pluggable solver.
#[async_trait]
pub trait Solver: Send + Sync + 'static {
    /// Stable identifier (the on-chain pubkey of the solver).
    fn address(&self) -> Address;

    /// Compute a solution for `batch`. Implementations should respect
    /// `deadline` (time remaining in the auction) and return their best
    /// effort within it.
    async fn solve(&self, batch: &Batch, deadline: Duration) -> Result<Solution>;
}

/// Reference solver — pure CoW matching, no AMM fallback.
pub struct ReferenceSolver {
    address: Address,
    matcher: Matcher,
}

impl ReferenceSolver {
    /// Construct a reference solver identified by `address`.
    pub fn new(address: Address) -> Self {
        Self { address, matcher: Matcher::default() }
    }
}

#[async_trait]
impl Solver for ReferenceSolver {
    fn address(&self) -> Address { self.address }

    async fn solve(&self, batch: &Batch, deadline: Duration) -> Result<Solution> {
        let matcher = self.matcher;
        let address = self.address;
        let batch_id = batch.id;
        let orders = batch.orders.clone();

        // Run CPU-bound work on a blocking thread so we never stall the reactor.
        let join = tokio::task::spawn_blocking(move || {
            matcher.match_batch(batch_id, address, &orders)
        });

        match tokio::time::timeout(deadline, join).await {
            Ok(Ok(sol)) => Ok(sol),
            Ok(Err(e))  => Err(OxDexError::Internal(format!("solver join: {e}"))),
            Err(_)      => Err(OxDexError::Internal("solver deadline exceeded".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxdex_types::BatchId;

    #[tokio::test]
    async fn solves_empty_batch() {
        let s = ReferenceSolver::new(Address::zero());
        let b = Batch { id: BatchId::new(), sealed_at: 0, orders: vec![] };
        let sol = s.solve(&b, Duration::from_millis(500)).await.unwrap();
        assert!(sol.trades.is_empty());
    }
}

