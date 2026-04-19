//! Shared domain types for OxDEX.
//!
//! This crate is dependency-light on purpose: every other crate depends on it,
//! so we keep it `no_std`-friendly-ish and avoid heavy runtime crates.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod address;
pub mod batch;
pub mod error;
pub mod order;
pub mod price;
pub mod solution;

pub use address::Address;
pub use batch::{Batch, BatchId};
pub use error::{OxDexError, Result};
pub use order::{Order, OrderId, OrderKind, OrderStatus, SignedOrder};
pub use price::Price;
pub use solution::{ClearingPrice, Solution, TradeExecution};
