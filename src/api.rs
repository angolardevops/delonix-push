use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{auth, dispatch, gateway, store, AppState};

pub(crate) type R = Result<Response, (StatusCode, Json<Value>)>;

pub(crate) fn err(code: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg })))
}
pub(crate) fn ise<E: std::fmt::Display>(e: E) -> (StatusCode, Json<Value>) {
    tracing::error!("{e}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "erro interno")
}

async fn project_of(st: &AppState, h: &HeaderMap) -> Result<Uuid, (StatusCode, Json<Value>)> {
    let key = auth::bearer(h)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "falta a chave de servidor"))?;
    let h = auth::hash(key);
    if let Some(p) = st.key_cache.get(&h) {
        return Ok(p);
    }
    let p = store::project_of_key(&st.db, key)
        .await
        .map_err(ise)?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "chave inválida"))?;
    st.key_cache.put(h, p);
    Ok(p)
}

pub fn router(st: AppState) -> Router {
    let console_dir = st.cfg.console_dir.clone();
    let api = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(metrics))
        .route("/admin/v1/projects/{id}/limits", put(set_limits))
        .route("/admin/v1/projects", post(create_project))
        .route("/v1/devices", post(create_device))
        .route("/v1/devices/{id}", delete(revoke_device))
        .route(
            "/v1/devices/{id}/topics/{topic}",
            put(subscribe).delete(unsubscribe),
        )
        .route("/v1/device/provider", put(set_provider))
        .route("/v1/messages", post(send))
        .route("/v1/messages/{id}", get(get_message))
        .route("/v1/connect", get(connect))
        .merge(crate::console::routes())
        .with_state(st);
    // A consola web (SPA): ficheiros estáticos, e qualquer outro caminho cai no index.html.
    match console_dir {
        Some(dir) => {
            let index = std::path::Path::new(&dir).join("index.html");
            api.fallback_service(
                tower_http::services::ServeDir::new(dir)
                    .fallback(tower_http::services::ServeFile::new(index)),
            )
        }
        None => api,
    }
}

#[derive(Deserialize)]
struct NewProject {
    name: String,
}

async fn create_project(State(st): State<AppState>, h: HeaderMap, Json(b): Json<NewProject>) -> R {
    if !auth::admin_ok(&h, &st.cfg.admin_token) {
        return Err(err(StatusCode::UNAUTHORIZED, "administração negada"));
    }
    if b.name.trim().is_empty() || b.name.len() > 100 {
        return Err(err(StatusCode::UNPROCESSABLE_ENTITY, "nome inválido"));
    }
    let (id, key) = store::create_project(&st.db, b.name.trim())
        .await
        .map_err(ise)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "project_id": id, "server_key": key })),
    )
        .into_response())
}

#[derive(Deserialize)]
struct NewDevice {
    platform: String,
}

async fn create_device(State(st): State<AppState>, h: HeaderMap, Json(b): Json<NewDevice>) -> R {
    let p = project_of(&st, &h).await?;
    if b.platform != "android" && b.platform != "ios" {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "platform: android|ios",
        ));
    }
    let (id, secret) = store::create_device(&st.db, p, &b.platform)
        .await
        .map_err(ise)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "device_id": id, "device_secret": secret })),
    )
        .into_response())
}

async fn revoke_device(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> R {
    let p = project_of(&st, &h).await?;
    if store::revoke_device(&st.db, p, id).await.map_err(ise)? {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(err(StatusCode::NOT_FOUND, "aparelho desconhecido"))
    }
}

pub(crate) fn valid_topic(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 100
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.~%-".contains(c))
}

async fn sub(st: AppState, h: HeaderMap, id: Uuid, topic: String, on: bool) -> R {
    let p = project_of(&st, &h).await?;
    if !valid_topic(&topic) {
        return Err(err(StatusCode::UNPROCESSABLE_ENTITY, "tópico inválido"));
    }
    if store::device_in_project(&st.db, p, id)
        .await
        .map_err(ise)?
        .is_none()
    {
        return Err(err(StatusCode::NOT_FOUND, "aparelho desconhecido"));
    }
    store::subscribe(&st.db, p, id, &topic, on)
        .await
        .map_err(ise)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
async fn subscribe(
    State(st): State<AppState>,
    h: HeaderMap,
    Path((id, t)): Path<(Uuid, String)>,
) -> R {
    sub(st, h, id, t, true).await
}
async fn unsubscribe(
    State(st): State<AppState>,
    h: HeaderMap,
    Path((id, t)): Path<(Uuid, String)>,
) -> R {
    sub(st, h, id, t, false).await
}

#[derive(Deserialize)]
struct ProviderBody {
    provider: String,
    token: Option<String>,
}

/// O próprio aparelho regista o token do FCM/APNs (ou volta à ligação própria).
async fn set_provider(State(st): State<AppState>, h: HeaderMap, Json(b): Json<ProviderBody>) -> R {
    let secret = auth::bearer(&h)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "falta o segredo do aparelho"))?;
    let dev = store::device_of_secret(&st.db, secret)
        .await
        .map_err(ise)?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "aparelho inválido"))?;
    let ok = match (b.provider.as_str(), b.token.as_deref()) {
        ("direct", None) => true,
        ("fcm", Some(t)) | ("apns", Some(t)) => !t.is_empty() && t.len() <= 4096,
        _ => false,
    };
    // Cada plataforma tem o seu fornecedor: um iPhone não tem token FCM.
    let coherent = !(b.provider == "fcm" && dev.platform != "android")
        && !(b.provider == "apns" && dev.platform != "ios");
    if !ok || !coherent {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "provider/token inválido para esta plataforma",
        ));
    }
    store::set_provider(&st.db, dev.id, &b.provider, b.token.as_deref())
        .await
        .map_err(ise)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub(crate) struct SendBody {
    pub(crate) device_id: Option<Uuid>,
    pub(crate) topic: Option<String>,
    pub(crate) payload: Value,
    pub(crate) priority: Option<String>,
    pub(crate) ttl_secs: Option<i64>,
    pub(crate) collapse_key: Option<String>,
    pub(crate) idempotency_key: Option<String>,
}

async fn send(State(st): State<AppState>, h: HeaderMap, Json(b): Json<SendBody>) -> R {
    let p = project_of(&st, &h).await?;
    // Protecção contra sobrecarga: com demasiados envios a decorrer recusa-se JÁ (503 + Retry-After), em vez de aceitar
    // e deixar a latência e a memória crescerem sem limite (medido a 2× a capacidade: p50 > 10 s).
    let Ok(_permit) = st.send_permits.clone().try_acquire_owned() else {
        crate::metrics::Metrics::inc(&st.metrics.shed);
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            [(axum::http::header::RETRY_AFTER, "1")],
            Json(json!({ "error": "servidor ocupado: tenta de novo", "retry_after_secs": 1 })),
        )
            .into_response());
    };
    do_send(&st, p, b).await
}

/// O envio, partilhado pela API de servidor e pelo «enviar mensagem de teste» da consola.
pub(crate) async fn do_send(st: &AppState, p: Uuid, b: SendBody) -> R {
    let size = serde_json::to_vec(&b.payload)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if size > st.cfg.max_payload_bytes {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload acima do limite",
        ));
    }
    let prio = b.priority.as_deref().unwrap_or("normal");
    if prio != "high" && prio != "normal" {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "priority: high|normal",
        ));
    }
    let ttl = b.ttl_secs.unwrap_or(st.cfg.default_ttl_secs);
    if ttl < 0 || ttl > st.cfg.max_ttl_secs {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ttl_secs fora do intervalo",
        ));
    }
    for k in [&b.collapse_key, &b.idempotency_key].into_iter().flatten() {
        if k.is_empty() || k.len() > 128 {
            return Err(err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "collapse_key/idempotency_key inválida",
            ));
        }
    }
    let targets: Vec<Uuid> = match (b.device_id, b.topic.as_deref()) {
        (Some(d), None) => {
            if store::device_in_project(&st.db, p, d)
                .await
                .map_err(ise)?
                .is_none()
            {
                return Err(err(StatusCode::NOT_FOUND, "aparelho desconhecido"));
            }
            vec![d]
        }
        (None, Some(t)) => {
            if !valid_topic(t) {
                return Err(err(StatusCode::UNPROCESSABLE_ENTITY, "tópico inválido"));
            }
            let v = store::topic_devices(&st.db, p, t, st.cfg.max_fanout + 1)
                .await
                .map_err(ise)?;
            if v.len() as i64 > st.cfg.max_fanout {
                return Err(err(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "tópico acima do limite de difusão",
                ));
            }
            v
        }
        _ => {
            return Err(err(
                StatusCode::UNPROCESSABLE_ENTITY,
                "indique device_id OU topic",
            ))
        }
    };
    // Limites do projecto: segundos e dia. Um tópico gasta uma unidade por aparelho.
    let (rate, quota) = match st.limits_cache.get(&p) {
        Some(v) => v,
        None => {
            let v: (Option<i32>, Option<i64>) =
                sqlx::query_as("SELECT rate_per_sec, daily_quota FROM projects WHERE id = $1")
                    .bind(p)
                    .fetch_one(&st.db)
                    .await
                    .map_err(ise)?;
            st.limits_cache.put(p, v);
            v
        }
    };
    let rate = rate.map_or(st.cfg.default_rate_per_sec, |v| v as u32);
    let quota = quota.map_or(st.cfg.default_daily_quota, |v| v as u64);
    if let Err(r) = st.limiter.check(p, targets.len() as u64, rate, quota).await {
        crate::metrics::Metrics::inc(&st.metrics.rate_limited);
        let (why, secs) = match r {
            crate::limits::Refused::RatePerSec { retry_after_secs } => (
                "limite de mensagens por segundo do projecto",
                retry_after_secs,
            ),
            crate::limits::Refused::DailyQuota { retry_after_secs } => {
                ("quota diária do projecto esgotada", retry_after_secs)
            }
        };
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, secs.to_string())],
            Json(json!({ "error": why, "retry_after_secs": secs })),
        )
            .into_response());
    }
    let mut ids = Vec::with_capacity(targets.len());
    for d in &targets {
        // Num tópico, a chave de idempotência é por aparelho: o mesmo pedido repetido não duplica.
        let (id, nova) = if b.idempotency_key.is_none() && b.collapse_key.is_none() {
            // Caminho comum: grava-se em grupo com as outras mensagens que chegam ao mesmo tempo (ver `batch`).
            let id = Uuid::new_v4();
            let row = crate::batch::NewRow {
                id,
                project: p,
                device: *d,
                priority: prio.to_string(),
                payload: b.payload.clone(),
                ttl_secs: ttl as f64,
            };
            if st.batch.insert.submit(row).await != Some(true) {
                return Err(ise("falha a gravar o lote de mensagens"));
            }
            (id, true)
        } else {
            store::enqueue(
                &st.db,
                &store::NewMessage {
                    project: p,
                    device: *d,
                    collapse_key: b.collapse_key.as_deref(),
                    priority: prio,
                    payload: &b.payload,
                    idempotency_key: b.idempotency_key.as_deref(),
                    ttl_secs: ttl,
                },
            )
            .await
            .map_err(ise)?
        };
        if nova {
            dispatch::deliver_to(st, id, *d).await;
        }
        ids.push(id);
    }
    st.metrics
        .enqueued
        .fetch_add(ids.len() as u64, std::sync::atomic::Ordering::Relaxed);
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "message_ids": ids, "accepted": ids.len() })),
    )
        .into_response())
}

async fn get_message(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> R {
    let p = project_of(&st, &h).await?;
    let m = store::message_in_project(&st.db, p, id)
        .await
        .map_err(ise)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "mensagem desconhecida"))?;
    Ok(Json(json!({
        "id": m.id, "device_id": m.device_id, "state": m.state, "attempts": m.attempts,
        "last_error": m.last_error, "created_at": m.created_at, "expires_at": m.expires_at,
    }))
    .into_response())
}

async fn connect(State(st): State<AppState>, h: HeaderMap, ws: WebSocketUpgrade) -> R {
    let secret = auth::bearer(&h)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "falta o segredo do aparelho"))?;
    let dev = store::device_of_secret(&st.db, secret)
        .await
        .map_err(ise)?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "aparelho inválido"))?;
    // Buffers pequenos: o valor por omissão (128 KiB de leitura e de escrita) custava ~190 KB por ligação
    // (medido); as mensagens e os acks são curtos.
    Ok(ws
        .read_buffer_size(4096)
        .write_buffer_size(4096)
        .max_write_buffer_size(256 * 1024)
        .max_message_size(64 * 1024)
        .on_upgrade(move |s| gateway::serve(st, dev, s)))
}

async fn metrics(State(st): State<AppState>, h: HeaderMap) -> axum::response::Response {
    let ok = !st.cfg.metrics_token.is_empty()
        && auth::bearer(&h).is_some_and(|t| {
            subtle::ConstantTimeEq::ct_eq(t.as_bytes(), st.cfg.metrics_token.as_bytes()).into()
        });
    if !ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        st.metrics.render(st.gateway.len()),
    )
        .into_response()
}

#[derive(Deserialize)]
struct Limits {
    rate_per_sec: Option<i32>,
    daily_quota: Option<i64>,
}

/// O operador da plataforma ajusta os limites de um projecto (plano). Um inquilino não os altera.
async fn set_limits(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<Limits>,
) -> R {
    if !auth::admin_ok(&h, &st.cfg.admin_token) {
        return Err(err(StatusCode::UNAUTHORIZED, "administração negada"));
    }
    if b.rate_per_sec.is_some_and(|v| v <= 0) || b.daily_quota.is_some_and(|v| v <= 0) {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "os limites têm de ser positivos",
        ));
    }
    st.limits_cache.clear();
    let n = sqlx::query("UPDATE projects SET rate_per_sec = $2, daily_quota = $3 WHERE id = $1")
        .bind(id)
        .bind(b.rate_per_sec)
        .bind(b.daily_quota)
        .execute(&st.db)
        .await
        .map_err(ise)?
        .rows_affected();
    if n == 0 {
        return Err(err(StatusCode::NOT_FOUND, "projecto desconhecido"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}
