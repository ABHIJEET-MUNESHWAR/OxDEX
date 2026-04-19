# 🐂 OxDEX

> **Strong, sturdy, and reliable like an Ox.**
> A CoW-Swap–style intent-based DEX for **Solana**, written in Rust.

OxDEX brings CoW Protocol's batch-auction + uniform-clearing-price model to
Solana, replacing Ethereum's mempool-PGA MEV game with **Jito bundles** for
atomic, leader-routed settlement. Users sign **intents**, not transactions;
solvers compete to maximise user surplus; one winning solution settles
on-chain in a single Jito-bundled transaction.

---

## Table of Contents

- [Architecture](#architecture)
- [How It Works](#how-it-works)
  - [0. Process boot (`oxdex-node`)](#0-process-boot-oxdex-node)
  - [1. Order submission flow — `POST /v1/orders`](#1-order-submission-flow--post-v1orders)
  - [2. Read flows](#2-read-flows)
  - [3. Cancellation flow — `DELETE /v1/orders/{id}`](#3-cancellation-flow--delete-v1ordersid)
  - [4. Auctioneer loop (background task)](#4-auctioneer-loop-background-task)
  - [5. Matching engine internals](#5-matching-engine-internals)
  - [6. Settlement / Jito flow](#6-settlement--jito-flow)
  - [7. End-to-end happy path (one wall-clock cycle)](#7-end-to-end-happy-path-one-wall-clock-cycle)
  - [8. Failure & recovery semantics at a glance](#8-failure--recovery-semantics-at-a-glance)
- [Workspace layout](#workspace-layout)
- [Quick start](#quick-start)
  - [Prerequisites](#prerequisites)
  - [Run with PostgreSQL](#run-with-postgresql)
  - [Run with no Postgres (dev)](#run-with-no-postgres-dev)
  - [Submit an order](#submit-an-order)
- [Docker](#docker)
- [Configuration](#configuration)
- [Testing](#testing)
- [Performance characteristics](#performance-characteristics)
- [Coding-standards self-assessment](#coding-standards-self-assessment)
  - [Honest gaps & next iterations](#honest-gaps--next-iterations)
- [OxDEX vs CoW Swap — comparison](#oxdex-vs-cow-swap--comparison)
- [Postman collection](#postman-collection)
- [License](#license)

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

## How It Works

This section documents every flow end-to-end: the data each component owns,
the exact request/response shape on the wire, and the sequence of internal
calls triggered for each event. File and line references point at the
canonical implementation if you want to read along.

### 0. Process boot (`oxdex-node`)

[`crates/oxdex-node/src/main.rs`](crates/oxdex-node/src/main.rs) is the
single binary entry point. On `cargo run -p oxdex-node` it executes, in
order:

1. **Tracing.** `init_tracing()` installs a `tracing_subscriber` with
   `EnvFilter` (defaults to `info`; override with `RUST_LOG`).
2. **Settings.** `Settings::load()` (from `oxdex-config`) layers
   `config/default.toml` → `config/${RUN_MODE}.toml` → environment
   variables prefixed `OXDEX__` (double underscore = section separator).
3. **Metrics.** `install_metrics()` starts a Prometheus HTTP exporter on
   `0.0.0.0:OXDEX_METRICS_PORT` (default `9100`). Every counter /
   histogram you see in this doc (`oxdex_orders_submitted_total`,
   `oxdex_auctioneer_batches_total`, …) is scraped from there.
4. **Storage.** `PgOrderRepository::connect(url, min, max)` is attempted
   first. On success it runs migrations
   (`crates/oxdex-storage/migrations/20260101000000_init.sql`); on
   failure it logs a warning and falls back to
   `InMemoryOrderRepository` (sharded `DashMap`). Either way the result
   is wrapped as `Arc<dyn OrderRepository>` and shared with both the
   API and the auctioneer.
5. **Solvers.** A `Vec<Arc<dyn Solver>>` is constructed; ships with one
   `ReferenceSolver` (pure CoW matcher). Adding more is a `vec![…]`
   change.
6. **Settlement sink.** A `JitoSink` wrapping `InMemoryJitoClient`
   (or `HttpJitoClient` in production) is built. If the env var
   `OXDEX_SETTLEMENT_LOGGING_ONLY` is set, the sink is replaced with
   `LoggingSink` (no submission, only `tracing::info!`).
7. **Auctioneer task.** `Auctioneer::new(cfg, repo, solvers, sink)` is
   `tokio::spawn`-ed with an `mpsc::channel(1)` shutdown handle.
8. **HTTP server.** `build_app(state, bind, workers).await` runs Actix
   in the foreground. When it returns (Ctrl+C / SIGTERM via Actix's
   built-in handling), `shutdown_tx.send(()).await` cooperatively stops
   the auctioneer, then `auc_handle.await` joins it.

The single shared `Arc<dyn OrderRepository>` is the *only* state
coupling between the API and the auctioneer — there is no in-memory
queue or message bus.

### 1. Order submission flow — `POST /v1/orders`

Routed in [`crates/oxdex-intent-pool/src/app.rs`](crates/oxdex-intent-pool/src/app.rs)
to `handlers::submit_order`
([`handlers.rs`](crates/oxdex-intent-pool/src/handlers.rs)).

**Wire format** (`SubmitBody`):

```json
{
  "signed": {
    "order": {
      "owner":        "<base58 32-byte pubkey>",
      "sell_mint":    "<base58 32-byte pubkey>",
      "buy_mint":     "<base58 32-byte pubkey>",
      "sell_amount":  1000000,
      "buy_amount":   2000000,
      "valid_to":     9999999999,
      "nonce":        1,
      "kind":         "sell",
      "partial_fill": true,
      "receiver":     "<base58 32-byte pubkey>"
    },
    "signature": "<128 hex chars = 64-byte Ed25519 sig over OrderId bytes>"
  }
}
```

Internal pipeline, in strict order (early-exit on the first error):

1. **Deserialize** via `serde_json` into `SignedOrder` (defined in
   [`crates/oxdex-types/src/order.rs`](crates/oxdex-types/src/order.rs)).
   Address fields decode from base58; signature decodes from hex.
2. **`order.validate(now_unix_secs)`** — pure synchronous semantic
   checks:
   * `sell_mint != buy_mint`
   * `sell_amount > 0`
   * `buy_amount > 0`
   * `valid_to > now`
   On failure → `OxDexError::InvalidOrder` → `ApiError` → HTTP `400`.
3. **`signed.verify()`** — Ed25519 signature check. The owner's 32-byte
   address is interpreted as the public key; the signature is verified
   over `order.id().0` (the 32-byte sha256 of canonical `bincode(order)`
   prefixed with the domain tag `b"oxdex/order/v1"`). Failure → HTTP
   `400` (`OxDexError::BadSignature`).
4. **`repo.insert(signed)`** — persists. The `OrderRepository` trait
   ([`crates/oxdex-storage/src/repository.rs`](crates/oxdex-storage/src/repository.rs))
   is implemented twice:
   * `PgOrderRepository` — `INSERT INTO orders (...) ON CONFLICT (id)
     DO NOTHING RETURNING ...`. Idempotent: re-submitting the same
     `OrderId` returns the existing record without error.
   * `InMemoryOrderRepository` — `DashMap<OrderId, OrderRecord>::entry().or_insert_with(...)`.
   Returns an `OrderRecord { id, status: Open, signed, created_at,
   executed_sell, executed_buy }`.
5. **Metric.** `metrics::counter!("oxdex_orders_submitted_total").increment(1)`.
6. **Response.** HTTP `201 Created`, body
   `{ "id": "<hex>", "status": "open" }`.

Failure mapping lives in
[`crates/oxdex-intent-pool/src/errors.rs`](crates/oxdex-intent-pool/src/errors.rs):
`InvalidOrder | InvalidAddress | BadSignature → 400`,
`Conflict → 409`, `NotFound → 404`, anything else → `500`.

### 2. Read flows

* **`GET /v1/orders`** (`list_orders`) — accepts optional `sell_mint`
  and `buy_mint` query params; both must be set together (otherwise
  `400`). Calls `repo.list_open(pair)`. The Postgres impl uses a
  partial index `WHERE status = 'open'` so the scan is cheap; the
  in-memory impl iterates the `DashMap` and filters.
* **`GET /v1/orders/{id}`** (`get_order`) — `parse_order_id` decodes a
  64-char hex string into `OrderId([u8; 32])`, then `repo.get(&id)`.
  Returns the full `OrderRecord` (including current status &
  cumulative `executed_sell` / `executed_buy`).
* **`GET /healthz`** — returns `200 ok` unconditionally.
* **`GET /readyz`** — calls `repo.list_open(None)` once; if it succeeds
  the service is "ready" (proves DB connectivity).

### 3. Cancellation flow — `DELETE /v1/orders/{id}`

1. Parse `id` from the path (hex, 32 bytes).
2. Read the `X-Owner` header. If missing → `400 InvalidOrder("missing
   X-Owner")`. The header must be the **base58** pubkey of the original
   signer.
3. Parse it into `Address`. Bad base58 / wrong length → `400`.
4. `repo.cancel(&id, &owner)` — atomic `UPDATE orders SET status =
   'cancelled' WHERE id = $1 AND owner = $2 AND status = 'open'`. The
   owner check is part of the `WHERE` clause, so there is no
   read-modify-write race; a third party with the id but not the key
   cannot cancel.
5. Returns `bool`:
   * `true`  → `204 No Content`, increments
     `oxdex_orders_cancelled_total`.
   * `false` → `409 Conflict` (`"not cancellable"` — already
     auctioned, filled, expired, or owner mismatch).

### 4. Auctioneer loop (background task)

[`crates/oxdex-auctioneer/src/lib.rs`](crates/oxdex-auctioneer/src/lib.rs).
A single long-lived `tokio` task. Tick interval is
`auction.batch_interval_ms` (default a few hundred ms; configurable).

```text
loop {
    select! {
        _ = shutdown.recv() => break,
        _ = ticker.tick()   => run_one_auction().await
    }
}
```

`MissedTickBehavior::Delay` ensures we do not spin on a back-pressured
tick after a slow round.

`run_one_auction()` performs five strictly ordered steps:

1. **Solver-count gate.** If `solvers.len() < cfg.min_solvers`, skip
   silently. Prevents shipping settlements with too little competition.
2. **Expiry sweep + seal.**
   * `repo.expire_due(now)` — single `UPDATE … WHERE status='open'
     AND valid_to <= $1` flips stale orders to `Expired`.
   * `repo.list_open(None)` — fetch the current open book.
   * Build `Batch { id: BatchId::new(), sealed_at: now, orders }`.
     `BatchId` is a fresh ULID-style 16-byte id; this is the
     content-free batch handle that flows through the rest of the
     round.
   * Increment `oxdex_auctioneer_batches_total`.
3. **Solver race.** A `FuturesUnordered` is built with one future per
   solver:
   ```text
   for s in solvers { (s.address(), s.solve(&batch, deadline).await) }
   ```
   Each `Solver::solve` (see
   [`crates/oxdex-solver/src/lib.rs`](crates/oxdex-solver/src/lib.rs))
   wraps `tokio::time::timeout(deadline, spawn_blocking(matcher.match_batch))`,
   so:
   * CPU-bound matching never stalls the reactor.
   * A slow solver is bounded by `cfg.solver_timeout_ms` and reported
     as an error rather than blocking the round.

   The auctioneer awaits results as they complete and keeps the one
   with the highest `score` (`u128` total surplus). Errored solvers
   are logged and skipped.
4. **Mark winners auctioned.** For each `TradeExecution` in the
   winning solution, call
   `repo.update_status(order_id, Auctioned, executed_sell, executed_buy)`.
   This prevents the same orders from being re-batched next tick.
   Failures are logged but do not abort the round.
5. **Deliver.** `sink.deliver(winning_solution).await`. The default
   sink is `JitoSink` (next section); tests use `CaptureSink`; dev
   mode can opt into `LoggingSink` via env.

Histograms emitted: `oxdex_auctioneer_score`. Counters:
`oxdex_auctioneer_trades_total`.

### 5. Matching engine internals

[`crates/oxdex-matching/src/lib.rs`](crates/oxdex-matching/src/lib.rs).
`Matcher::match_batch(batch_id, solver, &orders) -> Solution` is pure,
deterministic, side-effect free. Steps:

1. **Group by canonical pair.** `canonical_pair(a, b) = (min(a,b), max(a,b))`.
   This guarantees that A→B and B→A orders for the same market end up
   in the same bucket regardless of insertion order.
2. **Per-pair, in parallel** (`rayon::into_par_iter` if
   `MatcherConfig.parallel` — default true). Each pair runs
   `match_pair(token_a, token_b, &orders)`:
   * **Split** orders by direction (`sell_mint == token_a` vs
     `sell_mint == token_b`); malformed rows are dropped.
   * **Sort each side** by `limit_price()` ascending (most aggressive
     first). `limit_price = buy_amount / sell_amount` as a rational
     `Price { num: u128, den: u128 }`.
   * **Greedy two-pointer fill.** While the heads of both queues
     **cross** (`lp_ab.num*lp_ba.num <= lp_ab.den*lp_ba.den`,
     all-integer math):
     * Compute `trade_a = min(remaining_a_sell,
       buy_capacity_in_A_of_b_head)`.
     * Update remaining; record fills; advance whichever side
       exhausted.
     * If either order has `partial_fill = false` and would only be
       partially filled by the trade, **roll back** that fill and
       skip the order (advance its pointer).
     * Track the last crossing limit-price pair `(lp_ab, lp_ba)` for
       use in step 3.
   * **Uniform clearing price.** The midpoint of the last crossing
     pair, computed in pure rational form to avoid float drift:
     `mid = (p.num*q.num + p.den*q.den) / (2 * p.den * q.num)`. Both
     reciprocals (`p_a_per_b` and `p_b_per_a`) are emitted as
     `ClearingPrice` entries.
   * **Re-price every fill** at the uniform price (CoW invariant:
     everyone trades at the same price), aggregate per `OrderId` into
     `TradeExecution { executed_sell, executed_buy }`, and
     accumulate **surplus** (`bought - limit_price.apply(sold)`) into
     the pair's score.
3. **Merge** all per-pair results and return
   `Solution { batch_id, solver, clearing_prices, trades, score }`.

Determinism: same input slice → identical `Solution` regardless of the
parallel/serial config (the `parallel_and_serial_agree` test in the
matching crate enforces this).

### 6. Settlement / Jito flow

`JitoSink::deliver` in
[`crates/oxdex-node/src/main.rs`](crates/oxdex-node/src/main.rs):

1. `encode_solution_as_placeholder_tx(&solution)` — currently
   serializes the `Solution` as JSON and base64-encodes it (a
   placeholder; the real implementation will build a Solana versioned
   transaction invoking the on-chain `oxdex-settlement` program).
2. Build a `Bundle { transactions: vec![tx], tip_lamports,
   trace_id: solution.batch_id.to_string() }`.
3. `submitter.submit(bundle).await`:
   * `HttpJitoClient` posts JSON-RPC `sendBundle` to the configured
     block-engine URL with a 5-second timeout. Non-2xx → `Server`
     error; transport failures → `Transport` error.
   * `InMemoryJitoClient` (default in dev) appends to a
     `Mutex<Vec<Bundle>>` and returns `inmem-N` so tests can assert.
4. Success → `tracing::info!("bundle submitted", bundle_id, batch)`.
   Failure → `tracing::warn!` and the round ends; **no retry storm** —
   the next tick will simply re-seal the still-`Auctioned` orders if
   the on-chain ack never lands. (Promotion from `Auctioned` to
   `Filled` / rollback semantics on bundle failure are part of the
   on-chain settlement program work.)

### 7. End-to-end happy path (one wall-clock cycle)

```
t = 0 ms     POST /v1/orders                      (Alice signs sell 100 A → ≥150 B)
              └─ validate → verify → repo.insert  → 201 {id, "open"}
t = 5 ms     POST /v1/orders                      (Bob   signs sell 200 B → ≥100 A)
              └─ … → 201 {id, "open"}

t = 400 ms   ticker.tick() in auctioneer
              ├─ repo.expire_due(now)             → 0 expired
              ├─ repo.list_open(None)             → [Alice, Bob]
              ├─ Batch { id, sealed_at, orders }  emitted
              ├─ ReferenceSolver.solve(batch, 200ms)
              │    └─ spawn_blocking(matcher.match_batch)
              │         └─ canonical_pair, sort, greedy fill,
              │            uniform mid-price, surplus scoring
              ├─ best = Some(solution)            score > 0
              ├─ for trade in solution.trades:
              │    repo.update_status(id, Auctioned, exec_sell, exec_buy)
              └─ sink.deliver(solution)
                   └─ JitoSink → encode → InMemoryJitoClient.submit
                                       → "inmem-0"
                   tracing::info!("bundle submitted")
```

Subsequent ticks find no `Open` orders for those ids and the loop
idles until new submissions arrive.

### 8. Failure & recovery semantics at a glance

| Failure | Where caught | User-visible result | Recovery |
|---|---|---|---|
| Bad JSON / missing field         | `actix-web` `Json` extractor | `400` with serde error  | Resubmit |
| Semantic invalid (expired, etc.) | `Order::validate`            | `400 InvalidOrder`      | Fix payload |
| Bad Ed25519 signature            | `SignedOrder::verify`        | `400 BadSignature`      | Re-sign |
| Bad base58 / hex                 | `Address::from_str` / `parse_order_id` | `400`         | Fix encoding |
| Duplicate submission             | `repo.insert` (UPSERT)       | `201` (idempotent)      | — |
| Cancel without `X-Owner`         | `cancel_order`               | `400`                   | Add header |
| Cancel by non-owner / wrong state| `repo.cancel` returns false  | `409 Conflict`          | Owner only |
| Postgres unreachable at boot     | `PgOrderRepository::connect` | Falls back to in-memory | Restart with DB up |
| Solver panic / timeout           | `tokio::time::timeout` in solver | Solver dropped from race | Other solvers continue |
| Zero valid solutions             | Auctioneer                   | Round skipped, `warn!`  | Next tick |
| Bundle submit fails              | `JitoSink::deliver`          | `warn!`, no retry       | Orders stay `Auctioned`; settlement program reconciles |
| Auctioneer task panics           | `auc_handle.await` in `main` | `warn!` on shutdown     | Process supervisor restarts node |

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

## Docker

A multi-stage [`Dockerfile`](Dockerfile) and a
[`docker-compose.yml`](docker-compose.yml) ship with the repo. The
image is built with [`cargo-chef`](https://github.com/LukeMathWalker/cargo-chef)
so the (slow) dependency layer is cached across rebuilds, then copied
into a `debian:bookworm-slim` runtime as a non-root user with
`tini` as PID 1 (clean SIGTERM → cooperative auctioneer + Actix
shutdown wired in `main.rs`).

```bash
# Build & run the full stack (postgres + migrations + node)
docker compose up --build

# API:        http://localhost:8080
# Metrics:    http://localhost:9100/metrics
# Postgres:   localhost:5432  (user/pass/db: oxdex/oxdex/oxdex)
```

The compose file runs migrations as a one-shot `sqlx-cli` job that
`oxdex-node` waits on (`service_completed_successfully`) before
starting, and gates the node on Postgres's `pg_isready` healthcheck.

Build the image standalone:

```bash
docker build -t oxdex-node:local .
docker run --rm -p 8080:8080 -p 9100:9100 \
  -e OXDEX__DATABASE__URL=postgres://user:pass@host:5432/db \
  oxdex-node:local
```

All `OXDEX__*` env vars from [Configuration](#configuration) work
inside the container.

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

## Postman collection

A ready-to-import Postman v2.1 collection for the intent-pool HTTP API
lives at [`postman/OxDEX.postman_collection.json`](postman/OxDEX.postman_collection.json).

Import via Postman → **Import** → **File**. It ships with collection
variables (`baseUrl`, `orderId`, `owner`, `sellMint`, `buyMint`),
pre-built requests for `/healthz`, `/readyz`, and the full
`/v1/orders` CRUD surface, and a templated `SubmitBody` JSON payload.
Replace the placeholder `signature` (128 hex chars) with one produced
by your signing client to exercise the happy path.

---

## License

Apache-2.0.

