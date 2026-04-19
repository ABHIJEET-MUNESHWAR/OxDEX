//! Auctioneer service.
//!
//! Responsibilities:
//!  1. Periodically seal a [`Batch`] from the open orderbook.
//!  2. Race all registered [`Solver`]s in parallel.
//!  3. Pick the winning [`Solution`] (highest score among valid ones).
//!  4. Mark winning orders `Auctioned` in storage and hand the solution
//!     off to the settlement layer.
//!
//! Designed to be `tokio::spawn`-ed once at startup. Stops cleanly on
//! the first send error from its shutdown channel.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use futures::stream::{FuturesUnordered, StreamExt};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, info, instrument, warn};

use oxdex_config::AuctionSettings;
use oxdex_solver::Solver;
use oxdex_storage::OrderRepository;
use oxdex_types::{Batch, BatchId, OrderStatus, Solution};

/// Sink that receives every winning solution. The settlement / Jito layer
/// implements this trait. Decoupled so tests can capture solutions in-memory.
#[async_trait::async_trait]
pub trait SolutionSink: Send + Sync + 'static {
    /// Receive a winning solution.
    async fn deliver(&self, solution: Solution);
}

/// No-op sink — useful in unit tests / dev mode.
pub struct LoggingSink;
#[async_trait::async_trait]
impl SolutionSink for LoggingSink {
    async fn deliver(&self, solution: Solution) {
        info!(batch = %solution.batch_id, trades = solution.trades.len(),
              score = solution.score, "settlement (logging-sink): solution delivered");
    }
}

/// Auctioneer process.
pub struct Auctioneer {
    cfg: AuctionSettings,
    repo: Arc<dyn OrderRepository>,
    solvers: Vec<Arc<dyn Solver>>,
    sink: Arc<dyn SolutionSink>,
}

impl Auctioneer {
    /// Construct.
    pub fn new(
        cfg: AuctionSettings,
        repo: Arc<dyn OrderRepository>,
        solvers: Vec<Arc<dyn Solver>>,
        sink: Arc<dyn SolutionSink>,
    ) -> Self {
        Self {
            cfg,
            repo,
            solvers,
            sink,
        }
    }

    /// Run forever, sealing one batch per `batch_interval_ms`.
    /// Returns when `shutdown` resolves.
    pub async fn run(self, mut shutdown: mpsc::Receiver<()>) {
        let mut ticker = tokio::time::interval(Duration::from_millis(self.cfg.batch_interval_ms));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        info!(
            interval_ms = self.cfg.batch_interval_ms,
            "auctioneer started"
        );

        loop {
            tokio::select! {
                _ = shutdown.recv() => { info!("auctioneer shutting down"); break; }
                _ = ticker.tick() => {
                    if let Err(e) = self.run_one_auction().await {
                        warn!(error = %e, "auction round failed");
                    }
                }
            }
        }
    }

    #[instrument(skip(self))]
    async fn run_one_auction(&self) -> Result<(), oxdex_types::OxDexError> {
        if self.solvers.len() < self.cfg.min_solvers {
            debug!(
                have = self.solvers.len(),
                need = self.cfg.min_solvers,
                "skipping auction: not enough solvers"
            );
            return Ok(());
        }

        // 1. Seal the batch.
        let now = unix_secs();
        let _expired = self
            .repo
            .expire_due(now)
            .await
            .map_err(Into::<oxdex_types::OxDexError>::into)?;
        let open = self
            .repo
            .list_open(None)
            .await
            .map_err(Into::<oxdex_types::OxDexError>::into)?;
        if open.is_empty() {
            debug!("no open orders");
            return Ok(());
        }
        let batch = Batch {
            id: BatchId::new(),
            sealed_at: now,
            orders: open.iter().map(|r| r.signed.clone()).collect(),
        };
        info!(batch = %batch.id, n_orders = batch.orders.len(), "sealed batch");
        metrics::counter!("oxdex_auctioneer_batches_total").increment(1);

        // 2. Race solvers in parallel, time-boxed.
        let deadline = Duration::from_millis(self.cfg.solver_timeout_ms);
        let mut fut: FuturesUnordered<_> = self
            .solvers
            .iter()
            .map(|s| {
                let s = Arc::clone(s);
                let b = batch.clone();
                async move { (s.address(), s.solve(&b, deadline).await) }
            })
            .collect();

        let mut best: Option<Solution> = None;
        while let Some((addr, res)) = fut.next().await {
            match res {
                Ok(sol) => {
                    debug!(solver = %addr, score = sol.score, "received solution");
                    if best.as_ref().map(|b| sol.score > b.score).unwrap_or(true) {
                        best = Some(sol);
                    }
                }
                Err(e) => warn!(solver = %addr, error = %e, "solver failed"),
            }
        }

        // 3. Deliver winner.
        let Some(winner) = best else {
            warn!(batch = %batch.id, "no valid solutions");
            return Ok(());
        };

        // 4. Mark touched orders as Auctioned.
        for t in &winner.trades {
            if let Err(e) = self
                .repo
                .update_status(
                    &t.order_id,
                    OrderStatus::Auctioned,
                    Some(t.executed_sell),
                    Some(t.executed_buy),
                )
                .await
            {
                warn!(error = %e, "failed to mark order auctioned");
            }
        }
        metrics::counter!("oxdex_auctioneer_trades_total").increment(winner.trades.len() as u64);
        metrics::histogram!("oxdex_auctioneer_score").record(winner.score as f64);

        self.sink.deliver(winner).await;
        Ok(())
    }
}

fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxdex_solver::ReferenceSolver;
    use oxdex_storage::memory::InMemoryOrderRepository;
    use oxdex_types::{Address, Order, OrderKind, SignedOrder};
    use parking_lot::Mutex;

    struct CaptureSink(Mutex<Vec<Solution>>);
    #[async_trait::async_trait]
    impl SolutionSink for CaptureSink {
        async fn deliver(&self, s: Solution) {
            self.0.lock().push(s);
        }
    }

    fn order(owner: u8, sell: Address, buy: Address, sa: u64, ba: u64) -> SignedOrder {
        SignedOrder {
            order: Order {
                owner: Address([owner; 32]),
                sell_mint: sell,
                buy_mint: buy,
                sell_amount: sa,
                buy_amount: ba,
                valid_to: i64::MAX,
                nonce: 0,
                kind: OrderKind::Sell,
                partial_fill: true,
                receiver: Address([owner; 32]),
            },
            signature: [0u8; 64],
        }
    }

    #[tokio::test]
    async fn end_to_end_round() {
        let repo: Arc<dyn OrderRepository> = Arc::new(InMemoryOrderRepository::new());
        let a = Address([1u8; 32]);
        let b = Address([2u8; 32]);
        repo.insert(order(1, a, b, 100, 150)).await.unwrap();
        repo.insert(order(2, b, a, 200, 100)).await.unwrap();

        let sink = Arc::new(CaptureSink(Mutex::new(vec![])));
        let solvers: Vec<Arc<dyn Solver>> = vec![Arc::new(ReferenceSolver::new(Address::zero()))];

        let cfg = AuctionSettings {
            batch_interval_ms: 50,
            solver_timeout_ms: 200,
            min_solvers: 1,
        };
        let auc = Auctioneer::new(cfg, repo.clone(), solvers, sink.clone());
        auc.run_one_auction().await.unwrap();

        let captured = sink.0.lock();
        assert_eq!(captured.len(), 1);
        assert!(!captured[0].trades.is_empty());
    }
}
