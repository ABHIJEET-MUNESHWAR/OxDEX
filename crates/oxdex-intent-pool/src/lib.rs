//! Actix-Web HTTP API for the OxDEX intent pool.
//!
//! Endpoints:
//!  * `GET    /healthz`             — liveness
//!  * `GET    /readyz`              — readiness (storage round-trip)
//!  * `POST   /v1/orders`           — submit a [`SignedOrder`]
//!  * `GET    /v1/orders/{id}`      — fetch one
//!  * `GET    /v1/orders?status=open[&sell_mint=…&buy_mint=…]` — list
//!  * `DELETE /v1/orders/{id}`      — cancel (owner check via `X-Owner` header,
//!    in production this would be a signed nonce)
//!  * `GET    /metrics`             — Prometheus
//!
//! All handlers return JSON `ApiError { code, message }` on failure.
#![forbid(unsafe_code)]
#![allow(missing_docs)] // internal HTTP service crate

pub mod app;
pub mod errors;
pub mod handlers;

pub use app::{build_app, AppState};
