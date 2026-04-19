//! End-to-end smoke test: HTTP submit → auctioneer → in-memory bundle.
//!
//! Uses only the in-memory storage and Jito stubs — no external services.

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;

use oxdex_auctioneer::{Auctioneer, SolutionSink};
use oxdex_config::AuctionSettings;
use oxdex_jito_client::{Bundle, BundleSubmitter, InMemoryJitoClient};
use oxdex_solver::{ReferenceSolver, Solver};
use oxdex_storage::{memory::InMemoryOrderRepository, OrderRepository};
use oxdex_types::{Address, Order, OrderKind, SignedOrder, Solution};

struct JitoSink {
    submitter: Arc<InMemoryJitoClient>,
}
#[async_trait::async_trait]
impl SolutionSink for JitoSink {
    async fn deliver(&self, sol: Solution) {
        let tx = oxdex_jito_client::encode_solution_as_placeholder_tx(&sol);
        let _ = self.submitter.submit(Bundle {
            transactions: vec![tx],
            tip_lamports: 1_000,
            trace_id: sol.batch_id.to_string(),
        }).await;
    }
}

fn signed(sell: Address, buy: Address, sa: u64, ba: u64) -> SignedOrder {
    let sk = SigningKey::generate(&mut OsRng);
    let pk = sk.verifying_key();
    let owner = Address(pk.to_bytes());
    let order = Order {
        owner, sell_mint: sell, buy_mint: buy,
        sell_amount: sa, buy_amount: ba,
        valid_to: i64::MAX, nonce: 1,
        kind: OrderKind::Sell, partial_fill: true,
        receiver: owner,
    };
    let sig = sk.sign(&order.id().0);
    SignedOrder { order, signature: sig.to_bytes() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round_trip_submit_to_bundle() {
    let repo: Arc<dyn OrderRepository> = Arc::new(InMemoryOrderRepository::new());

    // Seed two crossing orders
    let a = Address([10u8; 32]);
    let b = Address([20u8; 32]);
    repo.insert(signed(a, b, 100, 150)).await.unwrap();
    repo.insert(signed(b, a, 200, 100)).await.unwrap();

    let jito = Arc::new(InMemoryJitoClient::new());
    let sink = Arc::new(JitoSink { submitter: jito.clone() });
    let solvers: Vec<Arc<dyn Solver>> = vec![Arc::new(ReferenceSolver::new(Address::zero()))];

    let cfg = AuctionSettings { batch_interval_ms: 50, solver_timeout_ms: 200, min_solvers: 1 };
    let auc = Auctioneer::new(cfg, repo.clone(), solvers, sink);

    // Run for ~250ms then verify a bundle landed.
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let h = tokio::spawn(auc.run(rx));
    tokio::time::sleep(Duration::from_millis(250)).await;
    let _ = tx.send(()).await;
    h.await.unwrap();

    let bundles = jito.submitted();
    assert!(!bundles.is_empty(), "expected at least one bundle");
}

