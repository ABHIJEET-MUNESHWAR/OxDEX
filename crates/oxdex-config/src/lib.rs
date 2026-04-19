//! Runtime configuration loader.
//!
//! Layered, in order of increasing precedence:
//!  1. Built-in defaults (this file).
//!  2. `config/default.toml` (optional).
//!  3. `config/{RUN_MODE}.toml` (optional).
//!  4. Environment variables prefixed `OXDEX__` (double-underscore = nesting).
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use serde::Deserialize;
use std::path::PathBuf;

use oxdex_types::OxDexError;

/// Top-level configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    /// HTTP server.
    pub server: ServerSettings,
    /// Database.
    pub database: DatabaseSettings,
    /// Auctioneer / batch settings.
    pub auction: AuctionSettings,
    /// Solana RPC.
    pub solana: SolanaSettings,
    /// Jito block-engine.
    pub jito: JitoSettings,
}

/// HTTP server settings.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerSettings {
    /// `host:port` to bind to.
    pub bind: String,
    /// Number of Actix worker threads.
    pub workers: usize,
}

/// PostgreSQL settings.
#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseSettings {
    /// Full Postgres URL (e.g. `postgres://user:pw@host/db`).
    pub url: String,
    /// Max pool connections.
    pub max_connections: u32,
    /// Min pool connections.
    pub min_connections: u32,
}

/// Auction / batching settings.
#[derive(Debug, Clone, Deserialize)]
pub struct AuctionSettings {
    /// How often to seal a new batch (milliseconds).
    pub batch_interval_ms: u64,
    /// How long solvers have to submit solutions (milliseconds).
    pub solver_timeout_ms: u64,
    /// Minimum number of registered solvers required to run an auction.
    pub min_solvers: usize,
}

/// Solana RPC settings.
#[derive(Debug, Clone, Deserialize)]
pub struct SolanaSettings {
    /// JSON-RPC endpoint.
    pub rpc_url: String,
}

/// Jito block-engine settings.
#[derive(Debug, Clone, Deserialize)]
pub struct JitoSettings {
    /// gRPC URL of the block engine.
    pub block_engine_url: String,
    /// Tip in lamports attached to each bundle.
    pub tip_lamports: u64,
}

impl Settings {
    /// Build [`Settings`] from layered sources.
    pub fn load() -> Result<Self, OxDexError> {
        // best-effort load .env (no-op in production)
        let _ = dotenvy::dotenv();

        let run_mode = std::env::var("RUN_MODE").unwrap_or_else(|_| "development".into());
        let cfg_dir = PathBuf::from(std::env::var("OXDEX_CONFIG_DIR").unwrap_or_else(|_| "config".into()));

        let mut builder = ::config::Config::builder()
            // baked-in defaults
            .set_default("server.bind", "0.0.0.0:8080").map_err(cfg_err)?
            .set_default("server.workers", 4i64).map_err(cfg_err)?
            .set_default("database.url", "postgres://oxdex:oxdex@localhost:5432/oxdex").map_err(cfg_err)?
            .set_default("database.max_connections", 20i64).map_err(cfg_err)?
            .set_default("database.min_connections", 2i64).map_err(cfg_err)?
            .set_default("auction.batch_interval_ms", 800i64).map_err(cfg_err)?
            .set_default("auction.solver_timeout_ms", 200i64).map_err(cfg_err)?
            .set_default("auction.min_solvers", 1i64).map_err(cfg_err)?
            .set_default("solana.rpc_url", "https://api.mainnet-beta.solana.com").map_err(cfg_err)?
            .set_default("jito.block_engine_url", "https://mainnet.block-engine.jito.wtf").map_err(cfg_err)?
            .set_default("jito.tip_lamports", 10_000i64).map_err(cfg_err)?;

        // optional file layers
        let default_file = cfg_dir.join("default.toml");
        if default_file.exists() {
            builder = builder.add_source(::config::File::from(default_file));
        }
        let mode_file = cfg_dir.join(format!("{run_mode}.toml"));
        if mode_file.exists() {
            builder = builder.add_source(::config::File::from(mode_file));
        }

        // env override: OXDEX__SECTION__KEY
        builder = builder.add_source(
            ::config::Environment::with_prefix("OXDEX")
                .separator("__")
                .try_parsing(true),
        );

        let cfg = builder.build().map_err(cfg_err)?;
        cfg.try_deserialize::<Settings>().map_err(cfg_err)
    }
}

fn cfg_err<E: std::fmt::Display>(e: E) -> OxDexError { OxDexError::Config(e.to_string()) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load() {
        // Ensure no env interferes with the "defaults only" case.
        std::env::remove_var("OXDEX_CONFIG_DIR");
        let s = Settings::load().expect("defaults must load");
        assert!(s.server.bind.contains(':'));
        assert!(s.database.max_connections >= s.database.min_connections);
        assert!(s.auction.batch_interval_ms > 0);
    }

    #[test]
    fn env_overrides() {
        std::env::set_var("OXDEX__SERVER__BIND", "127.0.0.1:9999");
        let s = Settings::load().unwrap();
        assert_eq!(s.server.bind, "127.0.0.1:9999");
        std::env::remove_var("OXDEX__SERVER__BIND");
    }
}

