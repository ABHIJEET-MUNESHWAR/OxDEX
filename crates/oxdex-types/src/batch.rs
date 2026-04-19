//! Batch and BatchId types — a snapshot of orders selected for one auction.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::order::SignedOrder;

/// UUID identifier for an auction batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BatchId(pub Uuid);

impl BatchId {
    /// Generate a new random v4 id.
    pub fn new() -> Self { Self(Uuid::new_v4()) }
}

impl Default for BatchId { fn default() -> Self { Self::new() } }

impl std::fmt::Display for BatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.fmt(f) }
}

/// A batch of orders presented to solvers as a single auction instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    /// Unique id.
    pub id: BatchId,
    /// Wall-clock unix-second the batch was sealed.
    pub sealed_at: i64,
    /// All eligible signed orders in this batch.
    pub orders: Vec<SignedOrder>,
}

impl Batch {
    /// Total number of orders.
    pub fn len(&self) -> usize { self.orders.len() }
    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool { self.orders.is_empty() }
}

