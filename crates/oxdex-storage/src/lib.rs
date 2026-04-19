//! Persistence layer for OxDEX.
//!
//! This crate exposes the [`OrderRepository`] trait and provides two
//! implementations:
//!  * [`postgres::PgOrderRepository`] — production store using SQLx.
//!  * [`memory::InMemoryOrderRepository`] — fast, in-process store used by tests
//!    and benchmarks. Identical semantics, no DB required.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod memory;
pub mod postgres;
pub mod repository;

pub use repository::{OrderRecord, OrderRepository, RepoError, RepoResult};
