//! Lock-free in-memory implementation of [`OrderRepository`].
//!
//! Uses [`dashmap`] for sharded concurrent hash-map access.
//! Suitable for tests, benchmarks, and dev mode without Postgres.

use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use std::sync::Arc;

use oxdex_types::{Address, OrderId, OrderStatus, SignedOrder};

use crate::repository::{OrderRecord, OrderRepository, RepoError, RepoResult};

/// In-memory store backed by `DashMap<OrderId, OrderRecord>`.
#[derive(Default, Clone)]
pub struct InMemoryOrderRepository {
    inner: Arc<DashMap<OrderId, OrderRecord>>,
}

impl InMemoryOrderRepository {
    /// Construct an empty repository.
    pub fn new() -> Self { Self::default() }

    /// Number of records currently held. O(1) amortised.
    pub fn len(&self) -> usize { self.inner.len() }
    /// Whether the repo is empty.
    pub fn is_empty(&self) -> bool { self.inner.is_empty() }
}

#[async_trait]
impl OrderRepository for InMemoryOrderRepository {
    async fn insert(&self, signed: SignedOrder) -> RepoResult<OrderRecord> {
        let id = signed.order.id();
        let now = Utc::now();
        let record = OrderRecord {
            id,
            signed,
            status: OrderStatus::Open,
            filled_sell: 0,
            filled_buy: 0,
            created_at: now,
            updated_at: now,
        };
        // try_insert pattern: dashmap doesn't have it; emulate.
        match self.inner.entry(id) {
            dashmap::mapref::entry::Entry::Vacant(v) => {
                v.insert(record.clone());
                Ok(record)
            }
            dashmap::mapref::entry::Entry::Occupied(o) => {
                if o.get().signed == record.signed {
                    Ok(o.get().clone())
                } else {
                    Err(RepoError::Duplicate(id))
                }
            }
        }
    }

    async fn get(&self, id: &OrderId) -> RepoResult<OrderRecord> {
        self.inner.get(id).map(|r| r.value().clone()).ok_or(RepoError::NotFound(*id))
    }

    async fn list_open(&self, pair: Option<(Address, Address)>) -> RepoResult<Vec<OrderRecord>> {
        let v = self.inner.iter()
            .filter(|r| r.value().status == OrderStatus::Open)
            .filter(|r| match pair {
                None => true,
                Some((s, b)) => r.value().signed.order.sell_mint == s
                              && r.value().signed.order.buy_mint == b,
            })
            .map(|r| r.value().clone())
            .collect();
        Ok(v)
    }

    async fn update_status(
        &self,
        id: &OrderId,
        status: OrderStatus,
        filled_sell: Option<u64>,
        filled_buy: Option<u64>,
    ) -> RepoResult<()> {
        let mut entry = self.inner.get_mut(id).ok_or(RepoError::NotFound(*id))?;
        entry.status = status;
        if let Some(s) = filled_sell { entry.filled_sell = s; }
        if let Some(b) = filled_buy  { entry.filled_buy  = b; }
        entry.updated_at = Utc::now();
        Ok(())
    }

    async fn cancel(&self, id: &OrderId, owner: &Address) -> RepoResult<bool> {
        let mut entry = self.inner.get_mut(id).ok_or(RepoError::NotFound(*id))?;
        if entry.signed.order.owner != *owner { return Ok(false); }
        if entry.status != OrderStatus::Open { return Ok(false); }
        entry.status = OrderStatus::Cancelled;
        entry.updated_at = Utc::now();
        Ok(true)
    }

    async fn expire_due(&self, now_unix_secs: i64) -> RepoResult<u64> {
        let mut count = 0u64;
        for mut e in self.inner.iter_mut() {
            if e.status == OrderStatus::Open && e.signed.order.valid_to <= now_unix_secs {
                e.status = OrderStatus::Expired;
                e.updated_at = Utc::now();
                count += 1;
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxdex_types::{Address, Order, OrderKind, SignedOrder};

    fn signed_for(now: i64, nonce: u64) -> SignedOrder {
        let o = Order {
            owner: Address([1u8; 32]),
            sell_mint: Address([2u8; 32]),
            buy_mint: Address([3u8; 32]),
            sell_amount: 1_000,
            buy_amount: 2_000,
            valid_to: now + 60,
            nonce,
            kind: OrderKind::Sell,
            partial_fill: true,
            receiver: Address([1u8; 32]),
        };
        SignedOrder { order: o, signature: [0u8; 64] }
    }

    #[tokio::test]
    async fn insert_and_get() {
        let r = InMemoryOrderRepository::new();
        let s = signed_for(1000, 1);
        let rec = r.insert(s.clone()).await.unwrap();
        assert_eq!(rec.status, OrderStatus::Open);
        let got = r.get(&rec.id).await.unwrap();
        assert_eq!(got.signed, s);
    }

    #[tokio::test]
    async fn cancel_only_by_owner() {
        let r = InMemoryOrderRepository::new();
        let s = signed_for(1000, 7);
        let rec = r.insert(s).await.unwrap();
        let other = Address([99u8; 32]);
        assert!(!r.cancel(&rec.id, &other).await.unwrap());
        assert!(r.cancel(&rec.id, &Address([1u8; 32])).await.unwrap());
    }

    #[tokio::test]
    async fn expire_marks_open_only() {
        let r = InMemoryOrderRepository::new();
        let s = signed_for(1000, 1);
        let rec = r.insert(s).await.unwrap();
        let n = r.expire_due(2000).await.unwrap();
        assert_eq!(n, 1);
        let got = r.get(&rec.id).await.unwrap();
        assert_eq!(got.status, OrderStatus::Expired);
    }

    #[tokio::test]
    async fn list_open_filters_by_pair() {
        let r = InMemoryOrderRepository::new();
        r.insert(signed_for(1000, 1)).await.unwrap();
        let v = r.list_open(Some((Address([2u8; 32]), Address([3u8; 32])))).await.unwrap();
        assert_eq!(v.len(), 1);
        let v = r.list_open(Some((Address([9u8; 32]), Address([3u8; 32])))).await.unwrap();
        assert!(v.is_empty());
    }
}

