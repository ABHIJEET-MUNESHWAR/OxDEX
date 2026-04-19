//! Repository trait used by the rest of the system.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use oxdex_types::{Address, OrderId, OrderStatus, OxDexError, SignedOrder};

/// Persistent record around a [`SignedOrder`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrderRecord {
    /// Content-addressed id (sha256 of canonical encoding).
    pub id: OrderId,
    /// The signed order.
    pub signed: SignedOrder,
    /// Lifecycle status.
    pub status: OrderStatus,
    /// Atomic units of `sell_mint` filled so far.
    pub filled_sell: u64,
    /// Atomic units of `buy_mint` delivered so far.
    pub filled_buy: u64,
    /// Insert time.
    pub created_at: DateTime<Utc>,
    /// Last update.
    pub updated_at: DateTime<Utc>,
}

/// Errors specific to the repository layer.
#[derive(Debug, Error)]
pub enum RepoError {
    /// Order with this id already exists.
    #[error("duplicate order: {0}")]
    Duplicate(OrderId),
    /// Order not found.
    #[error("not found: {0}")]
    NotFound(OrderId),
    /// Underlying storage error.
    #[error("backend error: {0}")]
    Backend(String),
}

impl From<RepoError> for OxDexError {
    fn from(e: RepoError) -> Self {
        match e {
            RepoError::Duplicate(id) => OxDexError::Conflict(format!("order {id} exists")),
            RepoError::NotFound(id)  => OxDexError::NotFound(format!("order {id}")),
            RepoError::Backend(s)    => OxDexError::Storage(s),
        }
    }
}

/// Convenience alias.
pub type RepoResult<T> = std::result::Result<T, RepoError>;

/// Storage abstraction. Object-safe so it can be `Arc<dyn OrderRepository>`.
#[async_trait]
pub trait OrderRepository: Send + Sync + 'static {
    /// Insert a brand-new order. Idempotent on the *exact* same record;
    /// returns [`RepoError::Duplicate`] if a different record already has this id.
    async fn insert(&self, signed: SignedOrder) -> RepoResult<OrderRecord>;

    /// Fetch one record by id.
    async fn get(&self, id: &OrderId) -> RepoResult<OrderRecord>;

    /// All currently-open orders, optionally filtered by sell+buy mint pair.
    async fn list_open(&self, pair: Option<(Address, Address)>) -> RepoResult<Vec<OrderRecord>>;

    /// Mutate status (and optionally fill amounts).
    async fn update_status(
        &self,
        id: &OrderId,
        status: OrderStatus,
        filled_sell: Option<u64>,
        filled_buy: Option<u64>,
    ) -> RepoResult<()>;

    /// Cancel an order on behalf of its owner. Returns `false` if not open.
    async fn cancel(&self, id: &OrderId, owner: &Address) -> RepoResult<bool>;

    /// Sweep expired open orders, marking them `Expired`. Returns count.
    async fn expire_due(&self, now_unix_secs: i64) -> RepoResult<u64>;
}

