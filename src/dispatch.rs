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

/// Canal do Postgres por onde uma instância pede a outra que entregue: `<node_id>:<message_id>`.
pub const CANAL: &str = "dpush_entrega";

/// Tenta entregar uma mensagem agora. Seguro de chamar de qualquer instância e de vários sítios:
/// - o aparelho está ligado a OUTRA instância → pede-lhe (NOTIFY) e não faz mais nada;
/// - está ligado a ESTA → reserva (`claim`, só um vence) e envia pela ligação;
/// - não está ligado em lado nenhum → fornecedor (FCM/APNs) se tiver token, senão fica na fila.
pub async fn deliver(st: &AppState, id: Uuid) {
    let Ok(Some(m)) = store::message(&st.db, id).await else {
        return;
    };
    if m.state != "queued" && m.state != "sent" {
        return;
    }
    route(st, id, m.device_id).await;
}

/// Que instância tem a ligação do aparelho, com cache curta (ver `AppState::presence_cache`).
async fn presence(st: &AppState, device: Uuid) -> Option<Uuid> {
    if let Some(p) = st.presence_cache.get(&device) {
        return p;
    }
    let p = store::presence_node(&st.db, device).await.ok().flatten();
    st.presence_cache.put(device, p);
    p
}

/// Decide por onde vai uma mensagem já gravada: ligação de OUTRA instância (avisa-a, em lote), ligação desta, fornecedor, ou fila.
async fn route(st: &AppState, id: Uuid, device: Uuid) {
    match presence(st, device).await {
        Some(node) if node != st.node_id => {
            let _ = st.batch.notify.submit((node, id, device)).await;
        }
        Some(_) if st.gateway.is_connected(device) => deliver_local(st, id, device).await,
        Some(_) => {
            // A cache diz que a ligação é desta instância mas já não é (o aparelho foi para outra): confirma na base, uma vez.
            st.presence_cache.remove(&device);
            match presence(st, device).await {
                Some(node) if node != st.node_id => {
                    let _ = st.batch.notify.submit((node, id, device)).await;
                }
                _ => deliver_provider(st, id).await,
            }
        }
        None => deliver_provider(st, id).await,
    }
}

/// Entrega pela ligação própria que ESTA instância tem (chamada também quando outra instância pede por NOTIFY).
/// Uma só consulta: reserva e marca como enviada (`claim_and_send`).
pub async fn deliver_local(st: &AppState, id: Uuid, device: Uuid) {
    if !st.gateway.is_connected(device) {
        return;
    }
    let lease = st.cfg.ack_timeout.as_secs_f64();
    let Some(Some(m)) = st.batch.claim_send.submit((id, lease)).await else {
        return;
    };
    if m.attempts > st.cfg.max_attempts {
        let _ = store::mark_failed(&st.db, m.id, "tentativas esgotadas").await;
        return;
    }
    // Se o envio falhar (fila cheia, ligação a cair), fica `sent` e o worker reenvia ao fim do lease.
    st.gateway.send(device, wire(&m));
}

/// Caminho rápido logo a seguir ao enfileirar: se o aparelho está ligado AQUI, entrega sem consultar a presença.
/// Senão, o caminho geral (outra instância, fornecedor, ou fica na fila).
pub async fn deliver_to(st: &AppState, id: Uuid, device: Uuid) {
    if st.gateway.is_connected(device) {
        deliver_local(st, id, device).await;
    } else {
        route(st, id, device).await;
    }
}

async fn deliver_provider(st: &AppState, id: Uuid) {
    let lease = st.cfg.ack_timeout.as_secs_f64();
    let Ok(Some(m)) = store::claim(&st.db, id, lease).await else {
        return;
    };
    if m.attempts >= st.cfg.max_attempts {
        let _ = store::mark_failed(&st.db, m.id, "tentativas esgotadas").await;
        return;
    }
    let Ok(Some(dev)) = store::device_in_project(&st.db, m.project_id, m.device_id).await else {
        let _ = store::mark_failed(&st.db, m.id, "aparelho desconhecido").await;
        return;
    };
    let (prov, token): (Option<Arc<dyn Provider>>, _) =
        match (dev.provider.as_str(), dev.provider_token.as_deref()) {
            ("fcm", Some(t)) => (
                crate::tenants::provider_for(st, m.project_id, "fcm").await,
                t,
            ),
            ("apns", Some(t)) => (
                crate::tenants::provider_for(st, m.project_id, "apns").await,
                t,
            ),
            _ => (None, ""),
        };
    let Some(prov) = prov else {
        // Sem caminho: fica na fila até ao aparelho ligar (o `claim` já adiou a próxima tentativa).
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
            crate::metrics::Metrics::inc(&st.metrics.provider_accepted);
        }
        Err(SendError::InvalidToken) => {
            // O token morreu: esquece-se, e a mensagem espera pela ligação própria.
            let _ = store::set_provider(&st.db, dev.id, "direct", None).await;
            let _ = store::retry_later(&st.db, m.id, "token do fornecedor inválido", 0.0).await;
        }
        Err(SendError::Permanent(e)) => {
            crate::metrics::Metrics::inc(&st.metrics.provider_failed);
            let _ = store::mark_failed(&st.db, m.id, &e).await;
        }
        Err(SendError::Transient(e)) => {
            let _ = store::retry_later(&st.db, m.id, &e, backoff(m.attempts)).await;
        }
    }
}

/// Escuta os pedidos das outras instâncias e entrega os que são para a que tem a ligação. Reconecta sozinho.
/// Um aviso de outra instância: «estas mensagens são para aparelhos ligados a ti». Entrega local direta (que também agrupa).
pub fn handle_notice(st: &AppState, items: Vec<(Uuid, Uuid)>) {
    for (id, device) in items {
        let st = st.clone();
        tokio::spawn(async move { deliver_local(&st, id, device).await });
    }
}

/// Lê um aviso `<id>@<aparelho>,<id>@<aparelho>,…`.
pub fn parse_notice(payload: &str) -> Vec<(Uuid, Uuid)> {
    payload
        .split(',')
        .filter_map(|t| {
            let (id, dev) = t.split_once('@')?;
            Some((Uuid::parse_str(id).ok()?, Uuid::parse_str(dev).ok()?))
        })
        .collect()
}

/// Escuta os avisos entre instâncias: pelo Redis se estiver ligado, senão pelo LISTEN do Postgres.
pub async fn run_listener(st: AppState) {
    if matches!(*st.bus, crate::bus::Bus::Redis { .. }) {
        return crate::bus::run_redis_subscriber(st).await;
    }
    run_pg_listener(st).await
}

async fn run_pg_listener(st: AppState) {
    loop {
        let Ok(mut l) = sqlx::postgres::PgListener::connect_with(&st.db).await else {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };
        if l.listen(CANAL).await.is_err() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        while let Ok(n) = l.recv().await {
            let Some((node, ids)) = n.payload().split_once(':') else {
                continue;
            };
            if node != st.node_id.to_string() {
                continue;
            }
            handle_notice(&st, parse_notice(ids));
        }
        // `recv` falhou (ligação perdida): volta a ligar. As mensagens perdidas apanha-as o `tick`.
    }
}

/// Reenvio, expiração e arrumação. Uma volta; `run_worker` repete-a.
pub async fn tick(st: &AppState) {
    let _ = store::expire_due(&st.db).await;
    store::presence_sweep(&st.db).await;
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
