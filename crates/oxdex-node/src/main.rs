//! OxDEX node binary.
//!
//! Boots, in this order:
//!  1. Tracing + Prometheus exporter
//!  2. Storage (Postgres if `OXDEX__DATABASE__URL` reachable; otherwise in-memory)
//!  3. Auctioneer (background tokio task)
//!  4. Actix-Web HTTP server (foreground, blocks until shutdown)
//!
//! Graceful shutdown: SIGINT / SIGTERM closes the auctioneer's shutdown channel
//! before Actix's `HttpServer` returns.

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use oxdex_auctioneer::{Auctioneer, LoggingSink, SolutionSink};
use oxdex_config::Settings;
use oxdex_intent_pool::{build_app, AppState};
use oxdex_jito_client::{
    encode_solution_as_placeholder_tx, Bundle, BundleSubmitter, InMemoryJitoClient,
};
use oxdex_solver::{ReferenceSolver, Solver};
use oxdex_storage::{
    memory::InMemoryOrderRepository, postgres::PgOrderRepository, OrderRepository,
};
use oxdex_types::{Address, Solution};

/// Settlement sink that wraps a [`BundleSubmitter`].
struct JitoSink<S: BundleSubmitter> {
    submitter: Arc<S>,
    tip_lamports: u64,
}

#[async_trait::async_trait]
impl<S: BundleSubmitter> SolutionSink for JitoSink<S> {
    async fn deliver(&self, solution: Solution) {
        let tx = encode_solution_as_placeholder_tx(&solution);
        let bundle = Bundle {
            transactions: vec![tx],
            tip_lamports: self.tip_lamports,
            trace_id: solution.batch_id.to_string(),
        };
        match self.submitter.submit(bundle).await {
            Ok(id) => info!(bundle_id = %id, batch = %solution.batch_id, "bundle submitted"),
            Err(e) => warn!(error = %e, "bundle submission failed"),
        }
    }
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let settings = Settings::load().map_err(|e| anyhow::anyhow!(e))?;
    info!(?settings, "loaded settings");

    install_metrics()?;

    // Storage — try Postgres, fall back to in-memory in dev.
    let repo: Arc<dyn OrderRepository> = match PgOrderRepository::connect(
        &settings.database.url,
        settings.database.min_connections,
        settings.database.max_connections,
    )
    .await
    {
        Ok(pg) => {
            if let Err(e) = pg.migrate().await {
                error!(error = %e, "migrations failed; aborting");
                return Err(anyhow::anyhow!("migration failed"));
            }
            info!("connected to postgres");
            Arc::new(pg)
        }
        Err(e) => {
            warn!(error = %e, "postgres unavailable — falling back to in-memory store (dev only)");
            Arc::new(InMemoryOrderRepository::new())
        }
    };

    // Solvers — start with a single reference solver.
    let solvers: Vec<Arc<dyn Solver>> = vec![Arc::new(ReferenceSolver::new(Address::zero()))];

    // Settlement sink — in-memory until real Jito wiring is added.
    let jito = Arc::new(InMemoryJitoClient::new());
    let sink: Arc<dyn SolutionSink> = Arc::new(JitoSink {
        submitter: jito.clone(),
        tip_lamports: settings.jito.tip_lamports,
    });
    // Allow opt-in pure-logging mode via env.
    let sink: Arc<dyn SolutionSink> = if std::env::var("OXDEX_SETTLEMENT_LOGGING_ONLY").is_ok() {
        Arc::new(LoggingSink)
    } else {
        sink
    };

    // Auctioneer
    let auc = Auctioneer::new(settings.auction.clone(), repo.clone(), solvers, sink);
    let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
    let auc_handle = tokio::spawn(auc.run(shutdown_rx));

    // HTTP server (foreground)
    let bind = settings.server.bind.clone();
    let workers = settings.server.workers;
    let app_state = AppState { repo: repo.clone() };
    info!(%bind, workers, "starting HTTP server");

    // Run server; on completion (or Ctrl+C inside Actix), trigger auctioneer shutdown.
    let serve_res = build_app(app_state, &bind, workers).await;

    let _ = shutdown_tx.send(()).await;
    if let Err(e) = auc_handle.await {
        warn!(error = %e, "auctioneer task panicked or was cancelled");
    }

    serve_res.map_err(Into::into)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true))
        .init();
}

fn install_metrics() -> anyhow::Result<()> {
    use metrics_exporter_prometheus::PrometheusBuilder;
    let port: u16 = std::env::var("OXDEX_METRICS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9100);
    PrometheusBuilder::new()
        .with_http_listener(([0, 0, 0, 0], port))
        .install()
        .map_err(|e| anyhow::anyhow!("metrics install: {e}"))?;
    info!(metrics_port = port, "Prometheus exporter listening");
    Ok(())
}
