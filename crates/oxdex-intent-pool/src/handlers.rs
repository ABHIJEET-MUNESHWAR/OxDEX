//! HTTP handlers.

use actix_web::{web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::instrument;

use oxdex_storage::{OrderRecord, OrderRepository};
use oxdex_types::{Address, OrderId, SignedOrder};

use crate::errors::ApiError;

#[derive(Clone)]
pub struct State {
    pub repo: Arc<dyn OrderRepository>,
}

/// `GET /healthz`
pub async fn healthz() -> impl Responder { HttpResponse::Ok().body("ok") }

/// `GET /readyz` — checks the repository can serve a list_open call cheaply.
#[instrument(skip(state))]
pub async fn readyz(state: web::Data<State>) -> Result<HttpResponse, ApiError> {
    state.repo.list_open(None).await.map_err(|e| ApiError(e.into()))?;
    Ok(HttpResponse::Ok().body("ready"))
}

#[derive(Debug, Deserialize)]
pub struct SubmitBody {
    pub signed: SignedOrder,
}

#[derive(Debug, Serialize)]
pub struct SubmitResponse {
    pub id: OrderId,
    pub status: &'static str,
}

/// `POST /v1/orders`
#[instrument(skip(state, body))]
pub async fn submit_order(
    state: web::Data<State>,
    body: web::Json<SubmitBody>,
) -> Result<HttpResponse, ApiError> {
    let signed = body.into_inner().signed;

    // 1. Cheap semantic checks (synchronous, deterministic).
    let now = unix_secs();
    signed.order.validate(now).map_err(ApiError)?;

    // 2. Cryptographic signature verification (constant-time-ish).
    signed.verify().map_err(ApiError)?;

    // 3. Persist.
    let rec = state.repo.insert(signed).await.map_err(|e| ApiError(e.into()))?;
    metrics::counter!("oxdex_orders_submitted_total").increment(1);

    Ok(HttpResponse::Created().json(SubmitResponse { id: rec.id, status: rec.status.as_str() }))
}

/// `GET /v1/orders/{id}` — id is hex.
#[instrument(skip(state))]
pub async fn get_order(
    state: web::Data<State>,
    path: web::Path<String>,
) -> Result<HttpResponse, ApiError> {
    let id = parse_order_id(&path.into_inner())?;
    let rec: OrderRecord = state.repo.get(&id).await.map_err(|e| ApiError(e.into()))?;
    Ok(HttpResponse::Ok().json(rec))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Optional sell mint (base58).
    pub sell_mint: Option<String>,
    /// Optional buy mint (base58).
    pub buy_mint: Option<String>,
}

/// `GET /v1/orders` (open orders only).
#[instrument(skip(state))]
pub async fn list_orders(
    state: web::Data<State>,
    q: web::Query<ListQuery>,
) -> Result<HttpResponse, ApiError> {
    let pair = match (&q.sell_mint, &q.buy_mint) {
        (Some(s), Some(b)) => {
            let s = s.parse::<Address>().map_err(ApiError)?;
            let b = b.parse::<Address>().map_err(ApiError)?;
            Some((s, b))
        }
        (None, None) => None,
        _ => return Err(ApiError(oxdex_types::OxDexError::InvalidOrder(
            "must specify both sell_mint and buy_mint, or neither".into()))),
    };
    let recs = state.repo.list_open(pair).await.map_err(|e| ApiError(e.into()))?;
    Ok(HttpResponse::Ok().json(recs))
}

/// Header schema (currently unused; kept for future strongly-typed extraction).
#[derive(Debug, Deserialize)]
pub struct CancelHeaders {
    /// Base58 owner pubkey supplied via `X-Owner` header.
    pub owner: String,
}

/// `DELETE /v1/orders/{id}` — header `X-Owner: <base58 pubkey>` required.
#[instrument(skip(state, req))]
pub async fn cancel_order(
    state: web::Data<State>,
    path: web::Path<String>,
    req: actix_web::HttpRequest,
) -> Result<HttpResponse, ApiError> {
    let id = parse_order_id(&path.into_inner())?;
    let owner_str = req.headers().get("x-owner")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError(oxdex_types::OxDexError::InvalidOrder("missing X-Owner".into())))?;
    let owner: Address = owner_str.parse().map_err(ApiError)?;
    let ok = state.repo.cancel(&id, &owner).await.map_err(|e| ApiError(e.into()))?;
    if ok {
        metrics::counter!("oxdex_orders_cancelled_total").increment(1);
        Ok(HttpResponse::NoContent().finish())
    } else {
        Err(ApiError(oxdex_types::OxDexError::Conflict("not cancellable".into())))
    }
}

fn parse_order_id(s: &str) -> Result<OrderId, ApiError> {
    let bytes = hex::decode(s).map_err(|e| ApiError(oxdex_types::OxDexError::InvalidOrder(format!("bad hex id: {e}"))))?;
    if bytes.len() != 32 {
        return Err(ApiError(oxdex_types::OxDexError::InvalidOrder("id must be 32 bytes".into())));
    }
    let mut a = [0u8; 32];
    a.copy_from_slice(&bytes);
    Ok(OrderId(a))
}

fn unix_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

