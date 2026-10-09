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
pub mod console;
pub mod dispatch;
pub mod gateway;
pub mod limits;
pub mod metrics;
pub mod providers;
pub mod seal;
pub mod store;
pub mod tenants;

use std::sync::Arc;

use sqlx::PgPool;

pub use providers::Provider;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    /// Identifica esta instância: quem tem a ligação de um aparelho é quem o entrega.
    pub node_id: uuid::Uuid,
    pub cfg: Arc<config::Config>,
    pub gateway: Arc<gateway::Registry>,
    pub fcm: Option<Arc<dyn Provider>>,
    pub apns: Option<Arc<dyn Provider>>,
    /// Fornecedores construídos a partir das credenciais de cada projecto (ver `tenants`).
    pub provider_cache: Arc<tenants::Cache>,
    pub limiter: Arc<limits::Limiter>,
    pub metrics: Arc<metrics::Metrics>,
}

impl AppState {
    pub fn new(db: PgPool, cfg: config::Config) -> Self {
        Self {
            db,
            node_id: uuid::Uuid::new_v4(),
            cfg: Arc::new(cfg),
            gateway: Arc::new(gateway::Registry::default()),
            fcm: None,
            apns: None,
            provider_cache: Arc::default(),
            limiter: Arc::new(limits::Limiter::memory()),
            metrics: Arc::default(),
        }
    }
}
