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
pub mod batch;
pub mod bus;
pub mod cache;
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

/// Limites de um projecto (`rate_per_sec`, `daily_quota`); `None` = os valores por omissão do servidor.
pub type ProjectLimits = (Option<i32>, Option<i64>);

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
    /// Chave de servidor (hash) → projecto, e limites por projecto: lidos a cada envio, validade curta (ver `cache`).
    pub key_cache: Arc<cache::Ttl<Vec<u8>, uuid::Uuid>>,
    pub limits_cache: Arc<cache::Ttl<uuid::Uuid, ProjectLimits>>,
    pub metrics: Arc<metrics::Metrics>,
    /// Licenças dos envios em curso (ver `Config::max_inflight_sends`).
    pub send_permits: Arc<tokio::sync::Semaphore>,
    /// Commit em grupo das escritas do caminho quente (ver `batch`).
    pub batch: Arc<batch::Batchers>,
    /// Como as instâncias se avisam umas às outras (Redis se configurado, senão o NOTIFY do Postgres).
    pub bus: Arc<bus::Bus>,
    /// Que instância tem a ligação de cada aparelho (validade 2 s). Se estiver desactualizada, a instância errada não entrega e
    /// o worker reencaminha a mensagem no segundo seguinte.
    /// (projecto, aparelho) já validados no último instante (validade 3 s): poupa uma consulta a cada envio.
    pub device_cache: Arc<cache::Ttl<(uuid::Uuid, uuid::Uuid), ()>>,
    pub presence_cache: Arc<cache::Ttl<uuid::Uuid, Option<uuid::Uuid>>>,
}

impl AppState {
    pub fn new(db: PgPool, cfg: config::Config) -> Self {
        let permits = cfg.max_inflight_sends;
        let bus = Arc::new(bus::Bus::Postgres);
        let metrics: Arc<metrics::Metrics> = Arc::default();
        Self {
            batch: Arc::new(batch::Batchers::new(db.clone(), bus.clone(), &metrics)),
            bus,
            db,
            node_id: uuid::Uuid::new_v4(),
            cfg: Arc::new(cfg),
            gateway: Arc::new(gateway::Registry::default()),
            fcm: None,
            apns: None,
            provider_cache: Arc::default(),
            limiter: Arc::new(limits::Limiter::memory()),
            key_cache: Arc::new(cache::Ttl::new(std::time::Duration::from_secs(3))),
            limits_cache: Arc::new(cache::Ttl::new(std::time::Duration::from_secs(5))),
            metrics,
            send_permits: Arc::new(tokio::sync::Semaphore::new(permits)),
            device_cache: Arc::new(cache::Ttl::new(std::time::Duration::from_secs(3))),
            presence_cache: Arc::new(cache::Ttl::new(std::time::Duration::from_secs(2))),
        }
    }

    /// Liga o Redis: limites partilhados entre instâncias e barramento entre instâncias (em vez do NOTIFY do Postgres).
    pub async fn use_redis(&mut self, url: &str) -> Result<(), redis::RedisError> {
        self.limiter = Arc::new(limits::Limiter::redis(url).await?);
        self.bus = Arc::new(bus::Bus::redis(url).await?);
        self.batch = Arc::new(batch::Batchers::new(
            self.db.clone(),
            self.bus.clone(),
            &self.metrics,
        ));
        Ok(())
    }
}
