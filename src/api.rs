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

type R = Result<Response, (StatusCode, Json<Value>)>;

fn err(code: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg })))
}
fn ise<E: std::fmt::Display>(e: E) -> (StatusCode, Json<Value>) {
    tracing::error!("{e}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "erro interno")
}

async fn project_of(st: &AppState, h: &HeaderMap) -> Result<Uuid, (StatusCode, Json<Value>)> {
    let key = auth::bearer(h)
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "falta a chave de servidor"))?;
    store::project_of_key(&st.db, key)
        .await
        .map_err(ise)?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "chave inválida"))
}

pub fn router(st: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
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
        .with_state(st)
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

fn valid_topic(t: &str) -> bool {
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
struct SendBody {
    device_id: Option<Uuid>,
    topic: Option<String>,
    payload: Value,
    priority: Option<String>,
    ttl_secs: Option<i64>,
    collapse_key: Option<String>,
    idempotency_key: Option<String>,
}

async fn send(State(st): State<AppState>, h: HeaderMap, Json(b): Json<SendBody>) -> R {
    let p = project_of(&st, &h).await?;
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
    let mut ids = Vec::with_capacity(targets.len());
    for d in &targets {
        // Num tópico, a chave de idempotência é por aparelho: o mesmo pedido repetido não duplica.
        let (id, nova) = store::enqueue(
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
        .map_err(ise)?;
        if nova {
            dispatch::deliver(&st, id).await;
        }
        ids.push(id);
    }
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
    Ok(ws.on_upgrade(move |s| gateway::serve(st, dev, s)))
}
