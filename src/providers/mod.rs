//! Fornecedores externos (FCM, APNs) atrás de um *trait*: o resto do código não sabe quem são.

pub mod apns;
pub mod fcm;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

/// O que se entrega a um fornecedor.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub id: Uuid,
    pub payload: Value,
    pub high_priority: bool,
    pub ttl_secs: i64,
    pub collapse_key: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// Tentar de novo mais tarde (5xx, rede, limite).
    #[error("transitório: {0}")]
    Transient(String),
    /// Nunca vai funcionar (pedido inválido, credencial).
    #[error("permanente: {0}")]
    Permanent(String),
    /// O fornecedor diz que o token já não existe: esquecer o token.
    #[error("token inválido")]
    InvalidToken,
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn send(&self, token: &str, msg: &Outgoing) -> Result<(), SendError>;
}

/// Classificação comum das respostas HTTP dos fornecedores.
pub(crate) fn classify(status: u16, body: &str) -> SendError {
    match status {
        404 | 410 => SendError::InvalidToken,
        408 | 429 | 500..=599 => SendError::Transient(format!("{status} {body}")),
        _ => SendError::Permanent(format!("{status} {body}")),
    }
}
