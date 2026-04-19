//! PostgreSQL implementation of [`OrderRepository`] via SQLx.
//!
//! The schema is defined in `crates/oxdex-storage/migrations/`. Run with:
//! `sqlx migrate run --source crates/oxdex-storage/migrations`.

use async_trait::async_trait;
use chrono::Utc;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use std::time::Duration;
use tracing::instrument;

use oxdex_types::{Address, OrderId, OrderStatus, SignedOrder};

use crate::repository::{OrderRecord, OrderRepository, RepoError, RepoResult};

/// PostgreSQL-backed repository.
#[derive(Clone)]
pub struct PgOrderRepository {
    pool: PgPool,
}

impl PgOrderRepository {
    /// Build a connection pool and run migrations.
    pub async fn connect(
        url: &str,
        min_conn: u32,
        max_conn: u32,
    ) -> RepoResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_conn)
            .min_connections(min_conn)
            .acquire_timeout(Duration::from_secs(5))
            .connect(url)
            .await
            .map_err(|e| RepoError::Backend(e.to_string()))?;
        Ok(Self { pool })
    }

    /// Run the bundled migrations.
    pub async fn migrate(&self) -> RepoResult<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|e| RepoError::Backend(e.to_string()))
    }

    /// Borrow the underlying pool (e.g. for health checks).
    pub fn pool(&self) -> &PgPool { &self.pool }
}

fn map_err(e: sqlx::Error) -> RepoError { RepoError::Backend(e.to_string()) }

fn row_to_record(row: &sqlx::postgres::PgRow) -> RepoResult<OrderRecord> {
    let id_bytes: Vec<u8> = row.try_get("id").map_err(map_err)?;
    if id_bytes.len() != 32 {
        return Err(RepoError::Backend(format!("bad id length {}", id_bytes.len())));
    }
    let mut id_arr = [0u8; 32];
    id_arr.copy_from_slice(&id_bytes);

    let signed_json: serde_json::Value = row.try_get("signed").map_err(map_err)?;
    let signed: SignedOrder = serde_json::from_value(signed_json)
        .map_err(|e| RepoError::Backend(format!("decode signed: {e}")))?;

    let status_str: String = row.try_get("status").map_err(map_err)?;
    let status = OrderStatus::from_db(&status_str)
        .ok_or_else(|| RepoError::Backend(format!("bad status {status_str}")))?;

    let filled_sell: i64 = row.try_get("filled_sell").map_err(map_err)?;
    let filled_buy:  i64 = row.try_get("filled_buy").map_err(map_err)?;

    Ok(OrderRecord {
        id: OrderId(id_arr),
        signed,
        status,
        filled_sell: filled_sell as u64,
        filled_buy:  filled_buy  as u64,
        created_at: row.try_get("created_at").map_err(map_err)?,
        updated_at: row.try_get("updated_at").map_err(map_err)?,
    })
}

#[async_trait]
impl OrderRepository for PgOrderRepository {
    #[instrument(skip(self, signed), fields(order_id = %signed.order.id()))]
    async fn insert(&self, signed: SignedOrder) -> RepoResult<OrderRecord> {
        let id = signed.order.id();
        let now = Utc::now();
        let signed_json = serde_json::to_value(&signed)
            .map_err(|e| RepoError::Backend(format!("encode signed: {e}")))?;
        let owner = signed.order.owner.as_bytes().to_vec();
        let sell_mint = signed.order.sell_mint.as_bytes().to_vec();
        let buy_mint  = signed.order.buy_mint.as_bytes().to_vec();
        let valid_to = signed.order.valid_to;

        let res = sqlx::query(
            r#"
            INSERT INTO orders
                (id, owner, sell_mint, buy_mint, valid_to, status,
                 filled_sell, filled_buy, signed, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, 'open', 0, 0, $6, $7, $7)
            ON CONFLICT (id) DO NOTHING
            RETURNING id, signed, status, filled_sell, filled_buy, created_at, updated_at
            "#,
        )
        .bind(id.0.to_vec())
        .bind(owner)
        .bind(sell_mint)
        .bind(buy_mint)
        .bind(valid_to)
        .bind(signed_json)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_err)?;

        match res {
            Some(row) => row_to_record(&row),
            // Conflict: fetch existing and compare.
            None => {
                let existing = self.get(&id).await?;
                if existing.signed == signed { Ok(existing) }
                else { Err(RepoError::Duplicate(id)) }
            }
        }
    }

    async fn get(&self, id: &OrderId) -> RepoResult<OrderRecord> {
        let row = sqlx::query(
            r#"SELECT id, signed, status, filled_sell, filled_buy, created_at, updated_at
               FROM orders WHERE id = $1"#,
        )
        .bind(id.0.to_vec())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_err)?;
        row.ok_or(RepoError::NotFound(*id)).and_then(|r| row_to_record(&r))
    }

    async fn list_open(&self, pair: Option<(Address, Address)>) -> RepoResult<Vec<OrderRecord>> {
        let rows = match pair {
            None => sqlx::query(
                r#"SELECT id, signed, status, filled_sell, filled_buy, created_at, updated_at
                   FROM orders WHERE status = 'open'"#,
            )
            .fetch_all(&self.pool).await,
            Some((s, b)) => sqlx::query(
                r#"SELECT id, signed, status, filled_sell, filled_buy, created_at, updated_at
                   FROM orders
                   WHERE status = 'open' AND sell_mint = $1 AND buy_mint = $2"#,
            )
            .bind(s.as_bytes().to_vec())
            .bind(b.as_bytes().to_vec())
            .fetch_all(&self.pool).await,
        }.map_err(map_err)?;

        rows.iter().map(row_to_record).collect()
    }

    async fn update_status(
        &self,
        id: &OrderId,
        status: OrderStatus,
        filled_sell: Option<u64>,
        filled_buy: Option<u64>,
    ) -> RepoResult<()> {
        let now = Utc::now();
        let res = sqlx::query(
            r#"UPDATE orders
               SET status = $2,
                   filled_sell = COALESCE($3, filled_sell),
                   filled_buy  = COALESCE($4, filled_buy),
                   updated_at = $5
               WHERE id = $1"#,
        )
        .bind(id.0.to_vec())
        .bind(status.as_str())
        .bind(filled_sell.map(|v| v as i64))
        .bind(filled_buy.map(|v| v as i64))
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(map_err)?;

        if res.rows_affected() == 0 { Err(RepoError::NotFound(*id)) } else { Ok(()) }
    }

    async fn cancel(&self, id: &OrderId, owner: &Address) -> RepoResult<bool> {
        let now = Utc::now();
        let res = sqlx::query(
            r#"UPDATE orders
               SET status = 'cancelled', updated_at = $3
               WHERE id = $1 AND owner = $2 AND status = 'open'"#,
        )
        .bind(id.0.to_vec())
        .bind(owner.as_bytes().to_vec())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(res.rows_affected() > 0)
    }

    async fn expire_due(&self, now_unix_secs: i64) -> RepoResult<u64> {
        let now = Utc::now();
        let res = sqlx::query(
            r#"UPDATE orders
               SET status = 'expired', updated_at = $2
               WHERE status = 'open' AND valid_to <= $1"#,
        )
        .bind(now_unix_secs)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(map_err)?;
        Ok(res.rows_affected())
    }
}

