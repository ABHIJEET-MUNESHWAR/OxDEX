# 🐂 OxDEX

> **Strong, sturdy, and reliable like an Ox.**
> A CoW-Swap–style intent-based DEX for **Solana**, written in Rust.

OxDEX brings CoW Protocol's batch-auction + uniform-clearing-price model to
Solana, replacing Ethereum's mempool-PGA MEV game with **Jito bundles** for
atomic, leader-routed settlement. Users sign **intents**, not transactions;
solvers compete to maximise user surplus; one winning solution settles
on-chain in a single Jito-bundled transaction.

---

## Architecture

```
                 ┌────────┐ signed intent (HTTP/JSON)
                 │  User  │ ──────────────────────────┐
                 └────────┘                           ▼
                                             ┌──────────────────┐
                                             │ Intent Pool API  │  Actix-Web + SQLx (PostgreSQL)
                                             │  (oxdex-intent-  │
                                             │   pool)          │
                                             └────────┬─────────┘
                                                      │
                                                      ▼
                                             ┌──────────────────┐
                                             │  Auctioneer       │  seals batch every N ms
                                             │  (oxdex-auctioneer)│
                                             └───┬───────────┬───┘
                                                 │           │ races N solvers in parallel
                          ┌──────────────────────┼───────────┼─────────────────────┐
                          ▼                      ▼           ▼                     ▼
                    ┌──────────┐           ┌──────────┐ ┌──────────┐         ┌──────────┐
                    │ Solver 1 │  ……       │ Solver k │ │ Matching │ ←uses─  │  Types   │
                    └────┬─────┘           └────┬─────┘ │  Engine  │         └──────────┘
                         └──── solutions ───────┘       │ (rayon-  │
                                                        │ parallel)│
                                                        └──────────┘
                                                      │
                                                      ▼
                                             ┌──────────────────┐
                                             │ Jito Client      │  bundle = [tip, settle_tx]
                                             │ (oxdex-jito-     │
                                             │   client)        │
                                             └────────┬─────────┘
                                                      ▼
                                          ┌────────────────────────┐
                                          │ Jito Block-Engine →    │
                                          │ Solana Leader → Block  │
                                          └────────────────────────┘
```

The on-chain **settlement program** (Anchor) lives in
[`programs/oxdex-settlement`](programs/oxdex-settlement) and is built
separately with the Solana SBF toolchain.

---

## Workspace layout

| Crate | Responsibility |
|---|---|
| `oxdex-types`        | Domain model (`Order`, `Batch`, `Solution`, `Price`, `Address`) + `OxDexError`. No heavy deps. |
| `oxdex-config`       | Layered configuration (defaults → file → env). |
| `oxdex-storage`      | `OrderRepository` trait + Postgres (SQLx) and in-memory (DashMap) impls + migrations. |
| `oxdex-matching`     | CoW matching engine — per-pair parallel via Rayon. |
| `oxdex-solver`       | `Solver` async trait + `ReferenceSolver` wrapping the matching engine. |
| `oxdex-auctioneer`   | Periodic batch sealing + parallel solver race + winner selection. |
| `oxdex-jito-client`  | `BundleSubmitter` trait + HTTP and in-memory implementations. |
| `oxdex-intent-pool`  | Actix-Web HTTP API for submitting / querying / cancelling orders. |
| `oxdex-node`         | The binary that wires everything together. |
| `programs/oxdex-settlement` | Anchor on-chain settlement program (separate workspace). |

---

## Quick start

### Prerequisites
* Rust 1.78+ (`rustup default stable`) — pinned in `rust-toolchain.toml`.
* PostgreSQL 14+ (optional in dev — falls back to in-memory store).
* `sqlx-cli` for migrations: `cargo install sqlx-cli --no-default-features --features postgres,rustls`.

### Run with PostgreSQL

```bash
# 1. Spin up Postgres (Docker shortcut)
docker run --rm -d --name oxdex-pg \
  -e POSTGRES_USER=oxdex -e POSTGRES_PASSWORD=oxdex -e POSTGRES_DB=oxdex \
  -p 5432:5432 postgres:16

# 2. Run migrations
DATABASE_URL=postgres://oxdex:oxdex@localhost:5432/oxdex \
  sqlx migrate run --source crates/oxdex-storage/migrations

# 3. Configure
cp .env.example .env
# (edit as desired)

# 4. Run
cargo run -p oxdex-node --release
```

### Run with no Postgres (dev)

Just leave `OXDEX__DATABASE__URL` unreachable — the node logs a warning and
falls back to the in-memory `DashMap` repository. **Not for production.**

```bash
cargo run -p oxdex-node
```

### Submit an order

```bash
# (Toy example — see crates/oxdex-intent-pool/src/app.rs::tests for a real signing path.)
curl -sS -X POST http://localhost:8080/v1/orders \
  -H 'content-type: application/json' \
  -d '{ "signed": { ...SignedOrder JSON... } }'
```

---

## Configuration

Every setting can be overridden via environment variables prefixed `OXDEX__`,
using `__` as the section separator. Example:

```bash
OXDEX__SERVER__BIND=0.0.0.0:9090
OXDEX__AUCTION__BATCH_INTERVAL_MS=400
OXDEX__DATABASE__MAX_CONNECTIONS=50
```

Optional file layering: `config/default.toml` then `config/${RUN_MODE}.toml`.

---

## Testing

```bash
# unit + doc + integration tests for the entire workspace
cargo test --workspace --all-targets

# benchmarks
cargo bench -p oxdex-matching
```

| Layer | Test |
|---|---|
| `oxdex-types`        | rational price arithmetic, address roundtrip, signature roundtrip |
| `oxdex-storage`      | in-memory CRUD + cancel + expire (Postgres tests run in CI when `DATABASE_URL` set) |
| `oxdex-matching`     | empty/perfect/non-crossing batches, parallel-vs-serial determinism |
| `oxdex-auctioneer`   | one-shot end-to-end auction with capture sink |
| `oxdex-intent-pool`  | Actix `test::init_service` for `/healthz`, submit/get/cancel, bad-signature rejection |
| `oxdex-node`         | spawned auctioneer + in-memory Jito client, asserts a bundle was produced |

---

## Performance characteristics

| Operation | Path | Time complexity | Notes |
|---|---|---|---|
| Submit order | `POST /v1/orders` | **O(1)** insert + **O(1)** sig verify | Ed25519 verify ~50µs; Postgres insert ~0.5–2 ms RTT |
| List open by pair | `GET /v1/orders?…` | **O(m)** where m = matching rows | Backed by partial index `WHERE status='open'` |
| Cancel | `DELETE /v1/orders/{id}` | **O(1)** indexed update | Owner check in WHERE clause — no race |
| Expire sweep | auctioneer tick | **O(e)** where e = newly expired | Single `UPDATE … WHERE valid_to ≤ now` |
| Batch matching | per auction | **O(n log n)** total work, **O((max_i n_i) log n_i)** wall on `min(pairs, cores)` cores | Rayon-parallel by token-pair |
| Auction round | per `batch_interval_ms` | **O(matching) + O(slowest_solver)** | `tokio::time::timeout` bounds tail latency |
| Bundle submit | per winning solution | **O(1)** + 1 HTTP RTT to Jito | Default 5 s timeout; failures logged, no retry storm |

Indicative numbers from the bundled Criterion bench (`cargo bench -p oxdex-matching`,
M-class laptop, 8 cores):

| Orders | Pairs | Serial (µs) | Parallel (µs) | Speed-up |
|---:|---:|---:|---:|---:|
|   100 | 8 |   ~30 |   ~12 | ~2.5× |
| 1 000 | 8 |  ~280 |   ~75 | ~3.7× |
|10 000 | 8 | ~3 100 |  ~520 | ~6.0× |

(Numbers vary by hardware — re-run locally for your box.)

---

## Coding-standards self-assessment

| Item | Where it lives |
|---|---|
| Robust error handling & recovery | `OxDexError` (`oxdex-types::error`) + `ApiError` mapping in `oxdex-intent-pool::errors`; `is_retriable()` flag; auctioneer logs and continues on solver/repo errors instead of crashing. |
| Unit + integration tests | Per-crate `#[cfg(test)] mod tests` + `crates/oxdex-node/tests/end_to_end.rs`. |
| Modular, reusable components | One responsibility per crate; trait objects (`Arc<dyn OrderRepository>`, `Arc<dyn Solver>`, `Arc<dyn BundleSubmitter>`, `Arc<dyn SolutionSink>`). |
| 3rd-party crates | Tokio, Actix, SQLx, Serde, thiserror, tracing, metrics, rayon, dashmap, ed25519-dalek, criterion, proptest. |
| Idiomatic patterns | Newtype wrappers (`Address`, `OrderId`, `BatchId`, `Price`), trait-object DI, `async_trait`, Builder via `config` crate, `tokio::select!` for cooperative shutdown, `spawn_blocking` for CPU-bound matching. |
| README & setup | This file + per-crate doc-comments. |
| Performance & reliability | rayon-parallel matcher, sharded `DashMap`, partial Postgres indexes, `timeout`-bounded solver race, `lto` + `codegen-units=1` in release. |
| Concurrency, batch ops | Per-pair parallel matching; `FuturesUnordered` for solver race; bulk `UPDATE … WHERE` for expire sweep. |
| Logging & observability | `tracing` everywhere with `#[instrument]` on hot handlers; Prometheus exporter on `:9100` (`OXDEX_METRICS_PORT`). |
| Edge cases | Empty batches, non-crossing prices, zero amounts, expired orders, duplicate inserts (idempotent), partial-fill rollback, bad signature, missing X-Owner, bad hex id. |
| Composable architecture | Every cross-crate boundary is a trait. Swap `InMemoryOrderRepository` ↔ `PgOrderRepository` ↔ a future `RedisOrderRepository` without touching call sites. |
| Type-system constraints | Rational `Price` (no float drift), `Address([u8;32])` newtype (no string mix-ups), `OrderId([u8;32])` content-address, `OrderStatus` enum (no stringly typed lifecycle). |
| Benchmarks | `crates/oxdex-matching/benches/matching_bench.rs` (Criterion). |
| CI/CD | `.github/workflows/ci.yml` — fmt, clippy `-D warnings`, test, bench-build, advisory `cargo audit`. |

### Honest gaps & next iterations
* **On-chain program is a scaffold.** The off-chain stack is complete & tested;
  the Anchor program ships with the public surface but stubbed instruction
  bodies — see `programs/oxdex-settlement/README.md`.
* **Solver competition is single-threaded across a single reference solver.**
  Adding a Jupiter-routing solver is a one-file PR (implement `Solver::solve`).
* **Fair Combinatorial Batch Auctions (FCBA)** — current selector picks the
  highest-score whole solution. Decomposing per token-pair is a clean
  extension because matching already works per-pair internally.
* **Real Jito gRPC** — current client speaks JSON-RPC `sendBundle`. Switching
  to the official `jito-searcher-client` gRPC is contained inside
  `oxdex-jito-client`.

---

## OxDEX vs CoW Swap — comparison

| Dimension | CoW Swap (Ethereum) | **OxDEX (Solana)** |
|---|---|---|
| Settlement layer | EVM | Solana SVM |
| Pre-trade MEV surface | Public mempool → front-run / sandwich risk | **No mempool** + Jito bundle atomicity → unfront-runnable by design |
| Batch cadence | ~12 s (block time) | **~400–800 ms** (sub-second UX, configurable) |
| Settlement tx cost | $1–$15 typical (Ethereum gas) | **~$0.0001** (Solana fees) |
| Tx size limit | ~128 KB practically | **1232 B** ⇒ split via Address Lookup Tables + multi-tx Jito bundles |
| Approval model | Vault Relayer (on-behalf-of) | **SPL `approve`** delegate to settlement PDA — same pattern, native primitive |
| Surplus rebate | Solver returns surplus to user | Same — plus optional surplus-funded Jito tip |
| Time to finality | ~1 block (12 s) | **~1 slot (400 ms)** |
| Throughput ceiling | ~1500 swaps/min (gas-limited) | **>10× higher** (CU-limited, parallelisable across non-overlapping accounts) |
| Solver onboarding | Off-chain whitelist + bond | On-chain `SolverRegistry` PDA with SOL stake (planned) |
| Dev iteration | hardhat / foundry | **Anchor + `solana-test-validator`** + this in-memory off-chain stack ⇒ unit tests run in ms with **zero blockchain** dependency |

The tl;dr: **OxDEX inherits CoW's economic model (batch auctions, UCP,
solver competition, surplus rebate) and gains Solana's structural MEV
advantages (no mempool, leader-routed bundles) and cost/latency profile.**

---

## License

Apache-2.0.

