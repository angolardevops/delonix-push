//! Commit em grupo: junta dezenas de operações concorrentes numa só instrução à base, em vez de uma por mensagem.
//!
//! Medido: cada mensagem custava 3 escritas separadas (INSERT, reserva+envio, ack) e o Postgres rende ~11 mil escritas/s,
//! pelo que o teto era ~1 000–1 200 mensagens/s. Agrupar não muda a garantia: quem pede só recebe resposta DEPOIS de o
//! lote ter sido gravado (o 202 continua a significar «está na base»). Custa até `LINGER` de latência a quem chega primeiro.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::{mpsc, oneshot, Semaphore};
use uuid::Uuid;

use crate::metrics::{Metrics, Stage};
use crate::store::Message;

const MAX_BATCH: usize = 128;
/// Quanto o primeiro item de um lote espera por companhia (`PUSH_BATCH_LINGER_MS`, 1 ms por omissão). Com poucas mensagens por
/// instância um valor baixo faz lotes de ~1 linha e o agrupamento deixa de render; mais espera = lotes maiores e mais latência.
fn linger() -> Duration {
    static L: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *L.get_or_init(|| {
        Duration::from_millis(
            std::env::var("PUSH_BATCH_LINGER_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
        )
    })
}
/// Lotes a gravar ao mesmo tempo (cada um usa uma ligação do pool: mais ligações pioraram nas medições).
const MAX_FLUSHES: usize = 4;

type Job<I, O> = (I, oneshot::Sender<O>);

/// Um agrupador: `submit` entrega um item e espera pelo resultado do lote em que foi.
pub struct Batcher<I, O> {
    tx: mpsc::Sender<Job<I, O>>,
    stage: Arc<Stage>,
}

impl<I: Send + 'static, O: Send + 'static> Batcher<I, O> {
    pub fn spawn<F, Fut>(stage: Arc<Stage>, flush: F) -> Self
    where
        F: Fn(Vec<I>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Vec<O>> + Send + 'static,
    {
        let (tx, mut rx) = mpsc::channel::<Job<I, O>>(8192);
        let flush = Arc::new(flush);
        let st = stage.clone();
        let permits = Arc::new(Semaphore::new(MAX_FLUSHES));
        tokio::spawn(async move {
            while let Some(first) = rx.recv().await {
                let mut batch = vec![first];
                let linger = tokio::time::sleep(linger());
                tokio::pin!(linger);
                while batch.len() < MAX_BATCH {
                    tokio::select! {
                        biased;
                        next = rx.recv() => match next { Some(j) => batch.push(j), None => break },
                        () = &mut linger => break,
                    }
                }
                let permit = permits
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("semáforo vivo");
                let flush = flush.clone();
                let st = st.clone();
                tokio::spawn(async move {
                    let (items, replies): (Vec<I>, Vec<oneshot::Sender<O>>) =
                        batch.into_iter().unzip();
                    let n = items.len() as u64;
                    let t0 = std::time::Instant::now();
                    let outs = flush(items).await;
                    st.flush.observe(t0.elapsed());
                    st.rows.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                    st.flushes
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    for (reply, out) in replies.into_iter().zip(outs) {
                        let _ = reply.send(out);
                    }
                    drop(permit);
                });
            }
        });
        Self { tx, stage }
    }

    pub async fn submit(&self, item: I) -> Option<O> {
        let t0 = std::time::Instant::now();
        let (r, rx) = oneshot::channel();
        self.tx.send((item, r)).await.ok()?;
        let out = rx.await.ok();
        self.stage.total.observe(t0.elapsed());
        out
    }
}

/// Mensagem a enfileirar (caminho comum: sem idempotência nem substituição). O `id` já vem escolhido.
pub struct NewRow {
    pub id: Uuid,
    pub project: Uuid,
    pub device: Uuid,
    pub priority: String,
    pub payload: Value,
    pub ttl_secs: f64,
}

pub struct Batchers {
    pub insert: Batcher<NewRow, bool>,
    pub claim_send: Batcher<(Uuid, f64), Option<Message>>,
    pub ack: Batcher<(Uuid, Uuid), bool>,
    /// «Entrega esta mensagem» dirigido a OUTRA instância: um `pg_notify` por lote e por instância, não um por mensagem.
    pub notify: Batcher<(Uuid, Uuid, Uuid), ()>,
}

impl Batchers {
    pub fn new(db: PgPool, bus: Arc<crate::bus::Bus>, m: &Metrics) -> Self {
        let d1 = db.clone();
        let insert = Batcher::spawn(m.insert.clone(), move |rows: Vec<NewRow>| {
            let db = d1.clone();
            async move {
                let n = rows.len();
                let ok = sqlx::query(
                    "INSERT INTO messages (id, project_id, device_id, priority, payload, state, expires_at)
                     SELECT id, p, d, prio, payload, 'queued', now() + make_interval(secs => ttl)
                       FROM unnest($1::uuid[], $2::uuid[], $3::uuid[], $4::text[], $5::jsonb[], $6::float8[])
                            AS t(id, p, d, prio, payload, ttl)",
                )
                .bind(rows.iter().map(|r| r.id).collect::<Vec<_>>())
                .bind(rows.iter().map(|r| r.project).collect::<Vec<_>>())
                .bind(rows.iter().map(|r| r.device).collect::<Vec<_>>())
                .bind(rows.iter().map(|r| r.priority.clone()).collect::<Vec<_>>())
                .bind(rows.iter().map(|r| r.payload.clone()).collect::<Vec<_>>())
                .bind(rows.iter().map(|r| r.ttl_secs).collect::<Vec<_>>())
                .execute(&db)
                .await
                .is_ok();
                vec![ok; n]
            }
        });
        let d2 = db.clone();
        let claim_send = Batcher::spawn(m.claim.clone(), move |jobs: Vec<(Uuid, f64)>| {
            let db = d2.clone();
            async move {
                let lease = jobs.first().map_or(30.0, |j| j.1);
                let ids: Vec<Uuid> = jobs.iter().map(|j| j.0).collect();
                let rows: Vec<Message> = sqlx::query_as(&format!(
                    "UPDATE messages m SET state = 'sent', attempts = attempts + 1, sent_at = now(), last_error = NULL,
                            next_attempt_at = now() + make_interval(secs => $2)
                      FROM unnest($1::uuid[]) AS t(id)
                      WHERE m.id = t.id AND m.state IN ('queued','sent') AND m.expires_at > now() AND m.next_attempt_at <= now()
                      RETURNING {}",
                    crate::store::MSG_COLS_PREFIXED
                ))
                .bind(&ids)
                .bind(lease)
                .fetch_all(&db)
                .await
                .unwrap_or_default();
                let mut by_id: std::collections::HashMap<Uuid, Message> =
                    rows.into_iter().map(|m| (m.id, m)).collect();
                ids.iter().map(|id| by_id.remove(id)).collect()
            }
        });
        let d4 = db.clone();
        let notify = Batcher::spawn(m.notify.clone(), move |items: Vec<(Uuid, Uuid, Uuid)>| {
            let (db, bus) = (d4.clone(), bus.clone());
            async move {
                // Agrupa por instância de destino; cada aviso é `<id>@<aparelho>,…`: o destino não precisa de consultar a base para
                // saber o aparelho. 100 itens por aviso (o limite do NOTIFY do Postgres é 8000 bytes).
                let now_us = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_micros());
                let mut by_node: std::collections::BTreeMap<Uuid, Vec<String>> =
                    std::collections::BTreeMap::new();
                for (node, id, device) in &items {
                    by_node
                        .entry(*node)
                        .or_default()
                        .push(format!("{id}@{device}@{now_us}"));
                }
                let mut payloads: Vec<(Uuid, String)> = Vec::new();
                for (node, toks) in by_node {
                    for chunk in toks.chunks(80) {
                        payloads.push((node, chunk.join(",")));
                    }
                }
                bus.publish(&db, &payloads).await;
                vec![(); items.len()]
            }
        });
        let d3 = db;
        let ack = Batcher::spawn(m.ack.clone(), move |pairs: Vec<(Uuid, Uuid)>| {
            let db = d3.clone();
            async move {
                // Só o dono da mensagem a confirma: o par (id, aparelho) tem de casar.
                let done: Vec<Uuid> = sqlx::query_scalar(
                    "UPDATE messages m SET state = 'delivered', delivered_at = now()
                       FROM unnest($1::uuid[], $2::uuid[]) AS t(id, device)
                      WHERE m.id = t.id AND m.device_id = t.device AND m.state IN ('queued','sent')
                      RETURNING m.id",
                )
                .bind(pairs.iter().map(|p| p.0).collect::<Vec<_>>())
                .bind(pairs.iter().map(|p| p.1).collect::<Vec<_>>())
                .fetch_all(&db)
                .await
                .unwrap_or_default();
                let set: std::collections::HashSet<Uuid> = done.into_iter().collect();
                pairs.iter().map(|p| set.contains(&p.0)).collect()
            }
        });
        Self {
            insert,
            claim_send,
            ack,
            notify,
        }
    }
}
