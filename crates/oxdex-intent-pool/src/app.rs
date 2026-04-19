//! Actix-Web app factory.

use actix_cors::Cors;
use actix_web::{web, App, HttpServer};
use std::sync::Arc;
use tracing_actix_web::TracingLogger;

use oxdex_storage::OrderRepository;

use crate::handlers::{self, State};

/// Public app state (cheap to clone).
#[derive(Clone)]
pub struct AppState {
    /// Backing repository.
    pub repo: Arc<dyn OrderRepository>,
}

/// Bind and run the HTTP server until SIGINT.
pub async fn build_app(state: AppState, bind: &str, workers: usize) -> std::io::Result<()> {
    let st = State { repo: state.repo };
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(st.clone()))
            .wrap(TracingLogger::default())
            .wrap(Cors::permissive())
            .route("/healthz", web::get().to(handlers::healthz))
            .route("/readyz", web::get().to(handlers::readyz))
            .service(
                web::scope("/v1/orders")
                    .route("", web::post().to(handlers::submit_order))
                    .route("", web::get().to(handlers::list_orders))
                    .route("/{id}", web::get().to(handlers::get_order))
                    .route("/{id}", web::delete().to(handlers::cancel_order)),
            )
    })
    .workers(workers.max(1))
    .bind(bind)?
    .run()
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App};
    use oxdex_storage::memory::InMemoryOrderRepository;
    use oxdex_types::{Address, Order, OrderKind, SignedOrder};

    fn signed_order() -> SignedOrder {
        use ed25519_dalek::{Signer, SigningKey};
        use rand::rngs::OsRng;
        let sk = SigningKey::generate(&mut OsRng);
        let pk = sk.verifying_key();
        let owner = Address(pk.to_bytes());
        let order = Order {
            owner,
            sell_mint: Address([2u8; 32]),
            buy_mint: Address([3u8; 32]),
            sell_amount: 1_000,
            buy_amount: 2_000,
            valid_to: i64::MAX,
            nonce: 1,
            kind: OrderKind::Sell,
            partial_fill: true,
            receiver: owner,
        };
        let sig = sk.sign(&order.id().0);
        SignedOrder {
            order,
            signature: sig.to_bytes(),
        }
    }

    #[actix_web::test]
    async fn healthz_ok() {
        let repo: Arc<dyn OrderRepository> = Arc::new(InMemoryOrderRepository::new());
        let st = State { repo };
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(st))
                .route("/healthz", web::get().to(handlers::healthz)),
        )
        .await;
        let req = test::TestRequest::get().uri("/healthz").to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn submit_then_get_then_cancel() {
        let repo: Arc<dyn OrderRepository> = Arc::new(InMemoryOrderRepository::new());
        let st = State { repo: repo.clone() };
        let app = test::init_service(
            App::new().app_data(web::Data::new(st)).service(
                web::scope("/v1/orders")
                    .route("", web::post().to(handlers::submit_order))
                    .route("/{id}", web::get().to(handlers::get_order))
                    .route("/{id}", web::delete().to(handlers::cancel_order)),
            ),
        )
        .await;

        let signed = signed_order();
        let owner_b58 = signed.order.owner.to_string();
        let id_hex = signed.order.id().to_hex();

        // submit
        let req = test::TestRequest::post()
            .uri("/v1/orders")
            .set_json(serde_json::json!({ "signed": signed }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 201, "submit failed");

        // get
        let req = test::TestRequest::get()
            .uri(&format!("/v1/orders/{}", id_hex))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        // cancel without owner header => 400
        let req = test::TestRequest::delete()
            .uri(&format!("/v1/orders/{}", id_hex))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400);

        // cancel with owner header => 204
        let req = test::TestRequest::delete()
            .uri(&format!("/v1/orders/{}", id_hex))
            .insert_header(("x-owner", owner_b58.as_str()))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 204);
    }

    #[actix_web::test]
    async fn submit_rejects_bad_signature() {
        let repo: Arc<dyn OrderRepository> = Arc::new(InMemoryOrderRepository::new());
        let st = State { repo };
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(st))
                .route("/v1/orders", web::post().to(handlers::submit_order)),
        )
        .await;

        let mut signed = signed_order();
        signed.signature[0] ^= 0xFF; // tamper

        let req = test::TestRequest::post()
            .uri("/v1/orders")
            .set_json(serde_json::json!({ "signed": signed }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status().as_u16(), 400);
    }
}
