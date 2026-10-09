//! delonix-push — entrega de mensagens a aplicações móveis.
//!
//! Três caminhos para o mesmo aparelho, escolhidos por mensagem:
//! 1. ligação própria (WebSocket, `gateway`): entrega com *ack*, a única que não depende de terceiros;
//! 2. FCM (Android) e 3. APNs (iOS), atrás do *trait* `Provider`, para acordar uma app sem ligação viva.
//!
//! Garantia: **pelo menos uma vez** com *ack* na ligação própria (a app deduplica por `id`); os
//! adaptadores FCM/APNs só garantem «aceite pelo fornecedor» (estado `accepted`).

pub mod api;
pub mod auth;
pub mod config;
pub mod dispatch;
pub mod gateway;
pub mod providers;
pub mod store;

use std::sync::Arc;

use sqlx::PgPool;

pub use providers::Provider;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cfg: Arc<config::Config>,
    pub gateway: Arc<gateway::Registry>,
    pub fcm: Option<Arc<dyn Provider>>,
    pub apns: Option<Arc<dyn Provider>>,
}

impl AppState {
    pub fn new(db: PgPool, cfg: config::Config) -> Self {
        Self {
            db,
            cfg: Arc::new(cfg),
            gateway: Arc::new(gateway::Registry::default()),
            fcm: None,
            apns: None,
        }
    }
}
