//! Escolhe o caminho de cada mensagem: ligação viva → fornecedor → fica à espera.

use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::providers::{Outgoing, SendError};
use crate::store::{self, Message};
use crate::{AppState, Provider};

pub fn wire(m: &Message) -> String {
    serde_json::json!({
        "type": "message",
        "id": m.id,
        "payload": m.payload,
        "priority": m.priority,
        "collapse_key": m.collapse_key,
        "expires_at": m.expires_at,
    })
    .to_string()
}

fn backoff(attempts: i32) -> f64 {
    (2f64.powi(attempts.clamp(0, 8))).min(300.0)
}

/// Tenta entregar uma mensagem agora. Seguro de chamar de vários sítios: o `claim` garante um só vencedor.
pub async fn deliver(st: &AppState, id: Uuid) {
    let lease = st.cfg.ack_timeout.as_secs_f64();
    let Ok(Some(m)) = store::claim(&st.db, id, lease).await else {
        return;
    };
    if m.attempts >= st.cfg.max_attempts {
        let _ = store::mark_failed(&st.db, m.id, "tentativas esgotadas").await;
        return;
    }
    // 1. Ligação própria, se o aparelho está ligado a esta instância.
    if st.gateway.send(m.device_id, wire(&m)) {
        let _ = store::mark_sent(&st.db, m.id, lease).await;
        return;
    }
    // 2. Fornecedor, se o aparelho tem token.
    let Ok(Some(dev)) = store::device_in_project(&st.db, m.project_id, m.device_id).await else {
        let _ = store::mark_failed(&st.db, m.id, "aparelho desconhecido").await;
        return;
    };
    let (prov, token): (Option<&Arc<dyn Provider>>, _) =
        match (dev.provider.as_str(), dev.provider_token.as_deref()) {
            ("fcm", Some(t)) => (st.fcm.as_ref(), t),
            ("apns", Some(t)) => (st.apns.as_ref(), t),
            _ => (None, ""),
        };
    let Some(prov) = prov else {
        // 3. Sem caminho: fica na fila até ao aparelho ligar (o `claim` já adiou a próxima tentativa).
        return;
    };
    let out = Outgoing {
        id: m.id,
        payload: m.payload.clone(),
        high_priority: m.priority == "high",
        ttl_secs: (m.expires_at - chrono::Utc::now()).num_seconds().max(1),
        collapse_key: m.collapse_key.clone(),
    };
    match prov.send(token, &out).await {
        Ok(()) => {
            let _ = store::mark_accepted(&st.db, m.id).await;
        }
        Err(SendError::InvalidToken) => {
            // O token morreu: esquece-se, e a mensagem espera pela ligação própria.
            let _ = store::set_provider(&st.db, dev.id, "direct", None).await;
            let _ = store::retry_later(&st.db, m.id, "token do fornecedor inválido", 0.0).await;
        }
        Err(SendError::Permanent(e)) => {
            let _ = store::mark_failed(&st.db, m.id, &e).await;
        }
        Err(SendError::Transient(e)) => {
            let _ = store::retry_later(&st.db, m.id, &e, backoff(m.attempts)).await;
        }
    }
}

/// Reenvio, expiração e arrumação. Uma volta; `run_worker` repete-a.
pub async fn tick(st: &AppState) {
    let _ = store::expire_due(&st.db).await;
    if let Ok(ids) = store::due_ids(&st.db, 200).await {
        for id in ids {
            deliver(st, id).await;
        }
    }
}

pub async fn run_worker(st: AppState) {
    let mut iv = tokio::time::interval(st.cfg.worker_interval.max(Duration::from_millis(100)));
    loop {
        iv.tick().await;
        tick(&st).await;
    }
}
