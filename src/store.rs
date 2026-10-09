//! Acesso à base. Todas as consultas levam o `project_id`: um projecto nunca vê o de outro.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth;

#[derive(Debug, FromRow, Clone)]
pub struct Device {
    pub id: Uuid,
    pub project_id: Uuid,
    pub platform: String,
    pub provider: String,
    pub provider_token: Option<String>,
}

#[derive(Debug, FromRow, Clone)]
pub struct Message {
    pub id: Uuid,
    pub project_id: Uuid,
    pub device_id: Uuid,
    pub collapse_key: Option<String>,
    pub priority: String,
    pub payload: Value,
    pub state: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

const MSG_COLS: &str = "id, project_id, device_id, collapse_key, priority, payload, state, attempts, last_error, created_at, expires_at";

pub async fn create_project(db: &PgPool, name: &str) -> sqlx::Result<(Uuid, String)> {
    let pid = Uuid::new_v4();
    let key = auth::new_secret("dpk");
    let mut tx = db.begin().await?;
    sqlx::query("INSERT INTO projects (id, name) VALUES ($1, $2)")
        .bind(pid)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO project_keys (id, project_id, key_hash, label) VALUES ($1, $2, $3, 'inicial')",
    )
    .bind(Uuid::new_v4())
    .bind(pid)
    .bind(auth::hash(&key))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((pid, key))
}

/// O projecto a que pertence uma chave de servidor activa.
pub async fn project_of_key(db: &PgPool, key: &str) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar(
        "SELECT project_id FROM project_keys WHERE key_hash = $1 AND revoked_at IS NULL",
    )
    .bind(auth::hash(key))
    .fetch_optional(db)
    .await
}

pub async fn create_device(
    db: &PgPool,
    project: Uuid,
    platform: &str,
) -> sqlx::Result<(Uuid, String)> {
    let id = Uuid::new_v4();
    let secret = auth::new_secret("dpd");
    sqlx::query(
        "INSERT INTO devices (id, project_id, platform, secret_hash) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(project)
    .bind(platform)
    .bind(auth::hash(&secret))
    .execute(db)
    .await?;
    Ok((id, secret))
}

pub async fn device_of_secret(db: &PgPool, secret: &str) -> sqlx::Result<Option<Device>> {
    sqlx::query_as("SELECT id, project_id, platform, provider, provider_token FROM devices WHERE secret_hash = $1 AND revoked_at IS NULL")
        .bind(auth::hash(secret)).fetch_optional(db).await
}

pub async fn device_in_project(
    db: &PgPool,
    project: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<Device>> {
    sqlx::query_as("SELECT id, project_id, platform, provider, provider_token FROM devices WHERE id = $1 AND project_id = $2 AND revoked_at IS NULL")
        .bind(id).bind(project).fetch_optional(db).await
}

pub async fn revoke_device(db: &PgPool, project: Uuid, id: Uuid) -> sqlx::Result<bool> {
    let r = sqlx::query("UPDATE devices SET revoked_at = now() WHERE id = $1 AND project_id = $2 AND revoked_at IS NULL")
        .bind(id).bind(project).execute(db).await?;
    if r.rows_affected() > 0 {
        sqlx::query("UPDATE messages SET state = 'expired', last_error = 'aparelho revogado' WHERE device_id = $1 AND state IN ('queued','sent')")
            .bind(id).execute(db).await?;
    }
    Ok(r.rows_affected() > 0)
}

pub async fn set_provider(
    db: &PgPool,
    device: Uuid,
    provider: &str,
    token: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE devices SET provider = $2, provider_token = $3 WHERE id = $1")
        .bind(device)
        .bind(provider)
        .bind(token)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn touch_device(db: &PgPool, id: Uuid) {
    let _ = sqlx::query("UPDATE devices SET last_seen_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await;
}

pub async fn subscribe(
    db: &PgPool,
    project: Uuid,
    device: Uuid,
    topic: &str,
    on: bool,
) -> sqlx::Result<()> {
    if on {
        sqlx::query("INSERT INTO subscriptions (device_id, project_id, topic) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
            .bind(device).bind(project).bind(topic).execute(db).await?;
    } else {
        sqlx::query(
            "DELETE FROM subscriptions WHERE device_id = $1 AND project_id = $2 AND topic = $3",
        )
        .bind(device)
        .bind(project)
        .bind(topic)
        .execute(db)
        .await?;
    }
    Ok(())
}

pub async fn topic_devices(
    db: &PgPool,
    project: Uuid,
    topic: &str,
    limit: i64,
) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT s.device_id FROM subscriptions s JOIN devices d ON d.id = s.device_id
          WHERE s.project_id = $1 AND s.topic = $2 AND d.revoked_at IS NULL ORDER BY s.device_id LIMIT $3",
    )
    .bind(project).bind(topic).bind(limit).fetch_all(db).await
}

pub struct NewMessage<'a> {
    pub project: Uuid,
    pub device: Uuid,
    pub collapse_key: Option<&'a str>,
    pub priority: &'a str,
    pub payload: &'a Value,
    pub idempotency_key: Option<&'a str>,
    pub ttl_secs: i64,
}

/// Enfileira uma mensagem. Devolve `(id, nova)`; um pedido repetido (mesma `idempotency_key`) devolve o id
/// original e `nova = false`. A mesma `collapse_key` substitui a mensagem anterior ainda por entregar.
pub async fn enqueue(db: &PgPool, m: &NewMessage<'_>) -> sqlx::Result<(Uuid, bool)> {
    let mut tx = db.begin().await?;
    if let Some(k) = m.idempotency_key {
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM messages WHERE project_id = $1 AND device_id = $2 AND idempotency_key = $3",
        )
        .bind(m.project).bind(m.device).bind(k).fetch_optional(&mut *tx).await?;
        if let Some(id) = existing {
            return Ok((id, false));
        }
    }
    if let Some(c) = m.collapse_key {
        sqlx::query("UPDATE messages SET state = 'expired', last_error = 'substituída (collapse_key)' WHERE device_id = $1 AND collapse_key = $2 AND state IN ('queued','sent')")
            .bind(m.device).bind(c).execute(&mut *tx).await?;
    }
    let id = Uuid::new_v4();
    let r = sqlx::query(
        "INSERT INTO messages (id, project_id, device_id, collapse_key, priority, payload, idempotency_key, state, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'queued', now() + make_interval(secs => $8))
         ON CONFLICT DO NOTHING",
    )
    .bind(id).bind(m.project).bind(m.device).bind(m.collapse_key).bind(m.priority).bind(m.payload)
    .bind(m.idempotency_key).bind(m.ttl_secs as f64)
    .execute(&mut *tx).await?;
    if r.rows_affected() == 0 {
        // Corrida com outro pedido idêntico: o vencedor já gravou.
        drop(tx);
        let id: Uuid = sqlx::query_scalar("SELECT id FROM messages WHERE project_id = $1 AND device_id = $2 AND idempotency_key = $3")
            .bind(m.project).bind(m.device).bind(m.idempotency_key).fetch_one(db).await?;
        return Ok((id, false));
    }
    tx.commit().await?;
    Ok((id, true))
}

pub async fn message(db: &PgPool, id: Uuid) -> sqlx::Result<Option<Message>> {
    sqlx::query_as(&format!("SELECT {MSG_COLS} FROM messages WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn message_in_project(
    db: &PgPool,
    project: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<Message>> {
    sqlx::query_as(&format!(
        "SELECT {MSG_COLS} FROM messages WHERE id = $1 AND project_id = $2"
    ))
    .bind(id)
    .bind(project)
    .fetch_optional(db)
    .await
}

/// Mensagens em aberto de um aparelho, por ordem de chegada (usado ao ligar).
pub async fn open_for_device(db: &PgPool, device: Uuid) -> sqlx::Result<Vec<Message>> {
    sqlx::query_as(&format!(
        "SELECT {MSG_COLS} FROM messages WHERE device_id = $1 AND state IN ('queued','sent') AND expires_at > now() ORDER BY created_at, id"
    ))
    .bind(device).fetch_all(db).await
}

/// Reserva uma mensagem para entrega (lease): só um processo a ganha, mesmo com várias instâncias.
pub async fn claim(db: &PgPool, id: Uuid, lease_secs: f64) -> sqlx::Result<Option<Message>> {
    sqlx::query_as(&format!(
        "UPDATE messages SET next_attempt_at = now() + make_interval(secs => $2)
          WHERE id = $1 AND state IN ('queued','sent') AND expires_at > now() AND next_attempt_at <= now()
          RETURNING {MSG_COLS}"
    ))
    .bind(id).bind(lease_secs).fetch_optional(db).await
}

pub async fn due_ids(db: &PgPool, limit: i64) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT id FROM messages WHERE state IN ('queued','sent') AND next_attempt_at <= now() ORDER BY next_attempt_at LIMIT $1")
        .bind(limit).fetch_all(db).await
}

pub async fn expire_due(db: &PgPool) -> sqlx::Result<u64> {
    Ok(sqlx::query("UPDATE messages SET state = 'expired', last_error = 'TTL' WHERE state IN ('queued','sent') AND expires_at <= now()")
        .execute(db).await?.rows_affected())
}

pub async fn mark_sent(db: &PgPool, id: Uuid, retry_secs: f64) -> sqlx::Result<()> {
    sqlx::query("UPDATE messages SET state = 'sent', attempts = attempts + 1, sent_at = now(), last_error = NULL, next_attempt_at = now() + make_interval(secs => $2) WHERE id = $1 AND state IN ('queued','sent')")
        .bind(id).bind(retry_secs).execute(db).await?;
    Ok(())
}

pub async fn mark_accepted(db: &PgPool, id: Uuid) -> sqlx::Result<()> {
    sqlx::query("UPDATE messages SET state = 'accepted', attempts = attempts + 1, sent_at = now(), last_error = NULL WHERE id = $1 AND state IN ('queued','sent')")
        .bind(id).execute(db).await?;
    Ok(())
}

pub async fn mark_failed(db: &PgPool, id: Uuid, err: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE messages SET state = 'failed', last_error = $2 WHERE id = $1 AND state IN ('queued','sent')")
        .bind(id).bind(err).execute(db).await?;
    Ok(())
}

pub async fn retry_later(db: &PgPool, id: Uuid, err: &str, backoff_secs: f64) -> sqlx::Result<()> {
    sqlx::query("UPDATE messages SET attempts = attempts + 1, last_error = $2, state = 'queued', next_attempt_at = now() + make_interval(secs => $3) WHERE id = $1 AND state IN ('queued','sent')")
        .bind(id).bind(err).bind(backoff_secs).execute(db).await?;
    Ok(())
}

/// O *ack* do aparelho. Só o dono da mensagem a confirma.
pub async fn ack(db: &PgPool, device: Uuid, id: Uuid) -> sqlx::Result<bool> {
    let r = sqlx::query("UPDATE messages SET state = 'delivered', delivered_at = now() WHERE id = $1 AND device_id = $2 AND state IN ('queued','sent')")
        .bind(id).bind(device).execute(db).await?;
    Ok(r.rows_affected() > 0)
}

/// Regista que `node` tem a ligação viva do aparelho (substitui a de outra instância).
pub async fn presence_set(db: &PgPool, device: Uuid, node: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO device_connections (device_id, node_id) VALUES ($1, $2)
         ON CONFLICT (device_id) DO UPDATE SET node_id = $2, connected_at = now(), seen_at = now()",
    )
    .bind(device)
    .bind(node)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn presence_touch(db: &PgPool, device: Uuid, node: Uuid) {
    let _ = sqlx::query(
        "UPDATE device_connections SET seen_at = now() WHERE device_id = $1 AND node_id = $2",
    )
    .bind(device)
    .bind(node)
    .execute(db)
    .await;
}

/// Só apaga se a linha ainda é desta instância (uma ligação nova noutra não se desfaz).
pub async fn presence_clear(db: &PgPool, device: Uuid, node: Uuid) {
    let _ = sqlx::query("DELETE FROM device_connections WHERE device_id = $1 AND node_id = $2")
        .bind(device)
        .bind(node)
        .execute(db)
        .await;
}

/// A instância com a ligação VIVA do aparelho (heartbeat nos últimos 120 s), se houver.
pub async fn presence_node(db: &PgPool, device: Uuid) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar("SELECT node_id FROM device_connections WHERE device_id = $1 AND seen_at > now() - interval '120 seconds'")
        .bind(device).fetch_optional(db).await
}

pub async fn presence_sweep(db: &PgPool) {
    let _ = sqlx::query(
        "DELETE FROM device_connections WHERE seen_at <= now() - interval '120 seconds'",
    )
    .execute(db)
    .await;
}
