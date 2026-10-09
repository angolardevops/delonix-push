//! API de gestão (`/console/v1`): contas, organizações, projectos, chaves, aparelhos, mensagens e estatísticas.
//! É o que a consola web usa e o que uma CLI/Terraform usaria. Nada aqui está no caminho quente de uma mensagem.
//!
//! Autorização em duas camadas: a SESSÃO identifica a conta (`Bearer dps_…`); o PAPEL na organização do projecto
//! decide o que pode fazer (`viewer` < `developer` < `admin` < `owner`). Um projecto de outra organização é
//! indistinguível de um que não existe (404), para não revelar o inventário alheio.

// As consultas devolvem tuplas largas lidas directamente do SQL; uma `struct` por consulta só acrescentaria ruído.
#![allow(clippy::type_complexity)]

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::api::{do_send, err, ise, SendBody, R};
use crate::{auth, AppState};

const SESSION_HOURS: i64 = 24 * 7;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/console/v1/auth/register", post(register))
        .route("/console/v1/auth/login", post(login))
        .route("/console/v1/auth/logout", post(logout))
        .route("/console/v1/me", get(me))
        .route("/console/v1/orgs", get(list_orgs).post(create_org))
        .route(
            "/console/v1/orgs/{org}/projects",
            get(list_projects).post(create_project),
        )
        .route("/console/v1/projects/{id}", get(get_project))
        .route(
            "/console/v1/projects/{id}/keys",
            get(list_keys).post(create_key),
        )
        .route("/console/v1/projects/{id}/keys/{key}", delete(revoke_key))
        .route("/console/v1/projects/{id}/devices", get(list_devices))
        .route(
            "/console/v1/projects/{id}/devices/{device}",
            delete(revoke_device),
        )
        .route(
            "/console/v1/projects/{id}/messages",
            get(list_messages).post(test_send),
        )
        .route("/console/v1/projects/{id}/stats", get(stats))
        .route(
            "/console/v1/projects/{id}/credentials",
            get(list_credentials),
        )
        .route(
            "/console/v1/projects/{id}/credentials/{kind}",
            put(put_credential).delete(delete_credential),
        )
}

// ---------- papéis e sessão ----------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Role {
    Viewer,
    Developer,
    Admin,
    Owner,
}

impl Role {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "viewer" => Self::Viewer,
            "developer" => Self::Developer,
            "admin" => Self::Admin,
            "owner" => Self::Owner,
            _ => return None,
        })
    }
}

async fn account_of(st: &AppState, h: &HeaderMap) -> Result<Uuid, (StatusCode, Json<Value>)> {
    let t = auth::bearer(h).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "sessão em falta"))?;
    sqlx::query_scalar(
        "SELECT account_id FROM console_sessions WHERE token_hash = $1 AND expires_at > now()",
    )
    .bind(auth::hash(t))
    .fetch_optional(&st.db)
    .await
    .map_err(ise)?
    .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "sessão inválida ou expirada"))
}

async fn role_in(
    db: &PgPool,
    account: Uuid,
    org: Uuid,
) -> Result<Option<Role>, (StatusCode, Json<Value>)> {
    let r: Option<String> =
        sqlx::query_scalar("SELECT role FROM memberships WHERE account_id = $1 AND org_id = $2")
            .bind(account)
            .bind(org)
            .fetch_optional(db)
            .await
            .map_err(ise)?;
    Ok(r.as_deref().and_then(Role::parse))
}

/// O projecto, desde que a conta seja membro da organização dele com pelo menos `min`.
/// 404 tanto para «não existe» como para «não é teu»; 403 só quando é membro mas sem papel suficiente.
async fn project_access(
    st: &AppState,
    h: &HeaderMap,
    project: Uuid,
    min: Role,
) -> Result<(Uuid, Uuid, Uuid), (StatusCode, Json<Value>)> {
    let account = account_of(st, h).await?;
    let org: Option<Uuid> = sqlx::query_scalar("SELECT org_id FROM projects WHERE id = $1")
        .bind(project)
        .fetch_optional(&st.db)
        .await
        .map_err(ise)?
        .flatten();
    let not_found = || err(StatusCode::NOT_FOUND, "projecto desconhecido");
    let org = org.ok_or_else(not_found)?;
    let role = role_in(&st.db, account, org).await?.ok_or_else(not_found)?;
    if role < min {
        return Err(err(
            StatusCode::FORBIDDEN,
            "papel insuficiente para esta acção",
        ));
    }
    Ok((account, org, project))
}

async fn audit(
    db: &PgPool,
    org: Option<Uuid>,
    project: Option<Uuid>,
    account: Uuid,
    action: &str,
    detail: &str,
) {
    let _ = sqlx::query("INSERT INTO audit_log (org_id, project_id, account_id, action, detail) VALUES ($1,$2,$3,$4,$5)")
        .bind(org)
        .bind(project)
        .bind(account)
        .bind(action)
        .bind(detail)
        .execute(db)
        .await;
}

// ---------- contas ----------

#[derive(Deserialize)]
struct Credentials {
    email: String,
    password: String,
}

fn valid_email(e: &str) -> bool {
    e.len() <= 254
        && e.split_once('@').is_some_and(|(l, d)| {
            !l.is_empty()
                && d.contains('.')
                && !d.starts_with('.')
                && !e.contains(char::is_whitespace)
        })
}

async fn register(State(st): State<AppState>, Json(b): Json<Credentials>) -> R {
    if !st.cfg.console_open_registration {
        return Err(err(
            StatusCode::FORBIDDEN,
            "o registo de contas está fechado",
        ));
    }
    let email = b.email.trim().to_lowercase();
    if !valid_email(&email) {
        return Err(err(StatusCode::UNPROCESSABLE_ENTITY, "e-mail inválido"));
    }
    if b.password.chars().count() < 10 || b.password.len() > 256 {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "a senha tem de ter entre 10 e 256 caracteres",
        ));
    }
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(b.password.as_bytes(), &salt)
        .map_err(ise)?
        .to_string();
    let id = Uuid::new_v4();
    let r = sqlx::query("INSERT INTO accounts (id, email, password_hash) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(&email)
        .bind(&hash)
        .execute(&st.db)
        .await;
    match r {
        Ok(_) => Ok((
            StatusCode::CREATED,
            Json(json!({ "id": id, "email": email })),
        )
            .into_response()),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            // Não se confirma que o e-mail existe: a mesma resposta de um registo normal seria enganadora,
            // mas 409 é o que a consola precisa para dizer «já tem conta».
            Err(err(
                StatusCode::CONFLICT,
                "já existe uma conta com esse e-mail",
            ))
        }
        Err(e) => Err(ise(e)),
    }
}

async fn login(State(st): State<AppState>, Json(b): Json<Credentials>) -> R {
    let email = b.email.trim().to_lowercase();
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, password_hash FROM accounts WHERE email = $1")
            .bind(&email)
            .fetch_optional(&st.db)
            .await
            .map_err(ise)?;
    // Verifica sempre um hash (um postiço se a conta não existe): o tempo não diz se o e-mail existe.
    let dummy = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$Bm3ZxOZtJ6Zb8m6s8U8Zk0m1m0m3m1m0m3m1m0m3m1k";
    let (id, hash) = match &row {
        Some((id, h)) => (Some(*id), h.as_str()),
        None => (None, dummy),
    };
    let ok = PasswordHash::new(hash)
        .map(|p| {
            Argon2::default()
                .verify_password(b.password.as_bytes(), &p)
                .is_ok()
        })
        .unwrap_or(false);
    let Some(id) = id.filter(|_| ok) else {
        return Err(err(StatusCode::UNAUTHORIZED, "e-mail ou senha errados"));
    };
    let token = auth::new_secret("dps");
    sqlx::query("INSERT INTO console_sessions (token_hash, account_id, expires_at) VALUES ($1,$2, now() + make_interval(hours => $3))")
        .bind(auth::hash(&token))
        .bind(id)
        .bind(SESSION_HOURS as i32)
        .execute(&st.db)
        .await
        .map_err(ise)?;
    Ok(Json(json!({ "token": token, "account_id": id, "email": email })).into_response())
}

async fn logout(State(st): State<AppState>, h: HeaderMap) -> R {
    if let Some(t) = auth::bearer(&h) {
        let _ = sqlx::query("DELETE FROM console_sessions WHERE token_hash = $1")
            .bind(auth::hash(t))
            .execute(&st.db)
            .await;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn me(State(st): State<AppState>, h: HeaderMap) -> R {
    let a = account_of(&st, &h).await?;
    let email: String = sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
        .bind(a)
        .fetch_one(&st.db)
        .await
        .map_err(ise)?;
    Ok(Json(json!({ "id": a, "email": email })).into_response())
}

// ---------- organizações e projectos ----------

#[derive(Deserialize)]
struct Named {
    name: String,
}

async fn create_org(State(st): State<AppState>, h: HeaderMap, Json(b): Json<Named>) -> R {
    let a = account_of(&st, &h).await?;
    let name = b.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(err(StatusCode::UNPROCESSABLE_ENTITY, "nome inválido"));
    }
    let id = Uuid::new_v4();
    let mut tx = st.db.begin().await.map_err(ise)?;
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1,$2)")
        .bind(id)
        .bind(name)
        .execute(&mut *tx)
        .await
        .map_err(ise)?;
    sqlx::query("INSERT INTO memberships (account_id, org_id, role) VALUES ($1,$2,'owner')")
        .bind(a)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(ise)?;
    tx.commit().await.map_err(ise)?;
    audit(&st.db, Some(id), None, a, "org.criada", name).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "name": name, "role": "owner" })),
    )
        .into_response())
}

async fn list_orgs(State(st): State<AppState>, h: HeaderMap) -> R {
    let a = account_of(&st, &h).await?;
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT o.id, o.name, m.role FROM memberships m JOIN organizations o ON o.id = m.org_id WHERE m.account_id = $1 ORDER BY o.created_at",
    )
    .bind(a)
    .fetch_all(&st.db)
    .await
    .map_err(ise)?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, role)| json!({ "id": id, "name": name, "role": role }))
            .collect::<Vec<_>>(),
    )
    .into_response())
}

async fn org_role(
    st: &AppState,
    h: &HeaderMap,
    org: Uuid,
    min: Role,
) -> Result<Uuid, (StatusCode, Json<Value>)> {
    let a = account_of(st, h).await?;
    let role = role_in(&st.db, a, org)
        .await?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "organização desconhecida"))?;
    if role < min {
        return Err(err(
            StatusCode::FORBIDDEN,
            "papel insuficiente para esta acção",
        ));
    }
    Ok(a)
}

async fn create_project(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(org): Path<Uuid>,
    Json(b): Json<Named>,
) -> R {
    let a = org_role(&st, &h, org, Role::Admin).await?;
    let name = b.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(err(StatusCode::UNPROCESSABLE_ENTITY, "nome inválido"));
    }
    let (id, key) = crate::store::create_project(&st.db, name)
        .await
        .map_err(ise)?;
    sqlx::query("UPDATE projects SET org_id = $2 WHERE id = $1")
        .bind(id)
        .bind(org)
        .execute(&st.db)
        .await
        .map_err(ise)?;
    audit(&st.db, Some(org), Some(id), a, "projecto.criado", name).await;
    // A chave de servidor só se mostra agora.
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "name": name, "server_key": key })),
    )
        .into_response())
}

async fn list_projects(State(st): State<AppState>, h: HeaderMap, Path(org): Path<Uuid>) -> R {
    org_role(&st, &h, org, Role::Viewer).await?;
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM projects WHERE org_id = $1 ORDER BY created_at")
            .bind(org)
            .fetch_all(&st.db)
            .await
            .map_err(ise)?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name)| json!({ "id": id, "name": name }))
            .collect::<Vec<_>>(),
    )
    .into_response())
}

async fn get_project(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> R {
    let (_, org, p) = project_access(&st, &h, id, Role::Viewer).await?;
    let (name,): (String,) = sqlx::query_as("SELECT name FROM projects WHERE id = $1")
        .bind(p)
        .fetch_one(&st.db)
        .await
        .map_err(ise)?;
    let (devices,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM devices WHERE project_id = $1 AND revoked_at IS NULL")
            .bind(p)
            .fetch_one(&st.db)
            .await
            .map_err(ise)?;
    let (connected,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM device_connections c JOIN devices d ON d.id = c.device_id WHERE d.project_id = $1 AND c.seen_at > now() - interval '120 seconds'",
    )
    .bind(p)
    .fetch_one(&st.db)
    .await
    .map_err(ise)?;
    let (rate, quota): (Option<i32>, Option<i64>) =
        sqlx::query_as("SELECT rate_per_sec, daily_quota FROM projects WHERE id = $1")
            .bind(p)
            .fetch_one(&st.db)
            .await
            .map_err(ise)?;
    Ok(Json(json!({
        "id": p, "org_id": org, "name": name, "devices": devices, "connected": connected,
        "limits": {
            "rate_per_sec": rate.map_or(i64::from(st.cfg.default_rate_per_sec), i64::from),
            "daily_quota": quota.unwrap_or(st.cfg.default_daily_quota as i64),
        },
    }))
    .into_response())
}

// ---------- chaves ----------

#[derive(Deserialize)]
struct KeyBody {
    #[serde(default)]
    label: String,
}

async fn create_key(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<KeyBody>,
) -> R {
    let (a, org, p) = project_access(&st, &h, id, Role::Admin).await?;
    if b.label.chars().count() > 100 {
        return Err(err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "rótulo demasiado longo",
        ));
    }
    let key = auth::new_secret("dpk");
    let kid = Uuid::new_v4();
    sqlx::query("INSERT INTO project_keys (id, project_id, key_hash, label, created_by) VALUES ($1,$2,$3,$4,$5)")
        .bind(kid)
        .bind(p)
        .bind(auth::hash(&key))
        .bind(b.label.trim())
        .bind(a)
        .execute(&st.db)
        .await
        .map_err(ise)?;
    audit(
        &st.db,
        Some(org),
        Some(p),
        a,
        "chave.criada",
        &kid.to_string(),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": kid, "label": b.label.trim(), "key": key })),
    )
        .into_response())
}

async fn list_keys(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> R {
    let (_, _, p) = project_access(&st, &h, id, Role::Admin).await?;
    let rows: Vec<(Uuid, String, chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT id, label, created_at, revoked_at FROM project_keys WHERE project_id = $1 ORDER BY created_at")
            .bind(p)
            .fetch_all(&st.db)
            .await
            .map_err(ise)?;
    // Nunca o valor da chave: só existe em claro no momento da criação.
    Ok(Json(rows.into_iter().map(|(id, label, c, r)| json!({ "id": id, "label": label, "created_at": c, "revoked_at": r })).collect::<Vec<_>>()).into_response())
}

async fn revoke_key(
    State(st): State<AppState>,
    h: HeaderMap,
    Path((id, key)): Path<(Uuid, Uuid)>,
) -> R {
    let (a, org, p) = project_access(&st, &h, id, Role::Admin).await?;
    // Revogar a última chave activa deixaria o projecto sem forma de enviar: permite-se (rodar = criar outra primeiro).
    let n = sqlx::query("UPDATE project_keys SET revoked_at = now() WHERE id = $1 AND project_id = $2 AND revoked_at IS NULL")
        .bind(key)
        .bind(p)
        .execute(&st.db)
        .await
        .map_err(ise)?
        .rows_affected();
    if n == 0 {
        return Err(err(StatusCode::NOT_FOUND, "chave desconhecida"));
    }
    audit(
        &st.db,
        Some(org),
        Some(p),
        a,
        "chave.revogada",
        &key.to_string(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------- aparelhos, mensagens, estatísticas ----------

#[derive(Deserialize)]
struct Page {
    limit: Option<i64>,
    /// Cursor: o `created_at` (RFC 3339) do último item da página anterior.
    before: Option<chrono::DateTime<chrono::Utc>>,
    state: Option<String>,
}

fn page_limit(l: Option<i64>) -> i64 {
    l.unwrap_or(50).clamp(1, 200)
}

async fn list_devices(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<Page>,
) -> R {
    let (_, _, p) = project_access(&st, &h, id, Role::Viewer).await?;
    let rows: Vec<(Uuid, String, String, chrono::DateTime<chrono::Utc>, Option<chrono::DateTime<chrono::Utc>>, bool)> = sqlx::query_as(
        "SELECT d.id, d.platform, d.provider, d.created_at, d.last_seen_at,
                EXISTS (SELECT 1 FROM device_connections c WHERE c.device_id = d.id AND c.seen_at > now() - interval '120 seconds')
           FROM devices d
          WHERE d.project_id = $1 AND d.revoked_at IS NULL AND ($2::timestamptz IS NULL OR d.created_at < $2)
          ORDER BY d.created_at DESC LIMIT $3",
    )
    .bind(p)
    .bind(q.before)
    .bind(page_limit(q.limit))
    .fetch_all(&st.db)
    .await
    .map_err(ise)?;
    Ok(Json(rows.into_iter().map(|(id, platform, provider, c, seen, on)| json!({
        "id": id, "platform": platform, "provider": provider, "created_at": c, "last_seen_at": seen, "connected": on
    })).collect::<Vec<_>>()).into_response())
}

async fn revoke_device(
    State(st): State<AppState>,
    h: HeaderMap,
    Path((id, dev)): Path<(Uuid, Uuid)>,
) -> R {
    let (a, org, p) = project_access(&st, &h, id, Role::Developer).await?;
    if !crate::store::revoke_device(&st.db, p, dev)
        .await
        .map_err(ise)?
    {
        return Err(err(StatusCode::NOT_FOUND, "aparelho desconhecido"));
    }
    audit(
        &st.db,
        Some(org),
        Some(p),
        a,
        "aparelho.revogado",
        &dev.to_string(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn list_messages(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<Page>,
) -> R {
    let (_, _, p) = project_access(&st, &h, id, Role::Viewer).await?;
    if let Some(s) = &q.state {
        if ![
            "queued",
            "sent",
            "delivered",
            "accepted",
            "expired",
            "failed",
        ]
        .contains(&s.as_str())
        {
            return Err(err(StatusCode::UNPROCESSABLE_ENTITY, "estado inválido"));
        }
    }
    let rows: Vec<(Uuid, Uuid, String, String, i32, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, device_id, state, priority, attempts, last_error, created_at
           FROM messages
          WHERE project_id = $1 AND ($2::text IS NULL OR state = $2) AND ($3::timestamptz IS NULL OR created_at < $3)
          ORDER BY created_at DESC LIMIT $4",
    )
    .bind(p)
    .bind(&q.state)
    .bind(q.before)
    .bind(page_limit(q.limit))
    .fetch_all(&st.db)
    .await
    .map_err(ise)?;
    // O conteúdo (payload) NÃO se lista: é do remetente e pode ser sensível; a consola mostra estado e erro.
    Ok(Json(rows.into_iter().map(|(id, device_id, state, priority, attempts, last_error, created_at)| json!({
        "id": id, "device_id": device_id, "state": state, "priority": priority, "attempts": attempts,
        "last_error": last_error, "created_at": created_at
    })).collect::<Vec<_>>()).into_response())
}

async fn test_send(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<SendBody>,
) -> R {
    let (a, org, p) = project_access(&st, &h, id, Role::Developer).await?;
    audit(&st.db, Some(org), Some(p), a, "mensagem.teste", "").await;
    do_send(&st, p, b).await
}

async fn stats(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> R {
    let (_, _, p) = project_access(&st, &h, id, Role::Viewer).await?;
    // Últimas 24 h: por estado, e por hora (criadas), mais a latência de entrega (criada → confirmada).
    let by_state: Vec<(String, i64)> = sqlx::query_as(
        "SELECT state, count(*) FROM messages WHERE project_id = $1 AND created_at > now() - interval '24 hours' GROUP BY state",
    )
    .bind(p)
    .fetch_all(&st.db)
    .await
    .map_err(ise)?;
    let hourly: Vec<(chrono::DateTime<chrono::Utc>, i64)> = sqlx::query_as(
        "SELECT date_trunc('hour', created_at) AS h, count(*) FROM messages
          WHERE project_id = $1 AND created_at > now() - interval '24 hours' GROUP BY h ORDER BY h",
    )
    .bind(p)
    .fetch_all(&st.db)
    .await
    .map_err(ise)?;
    let lat: (Option<f64>, Option<f64>) = sqlx::query_as(
        "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY extract(epoch FROM delivered_at - created_at)),
                percentile_cont(0.95) WITHIN GROUP (ORDER BY extract(epoch FROM delivered_at - created_at))
           FROM messages WHERE project_id = $1 AND state = 'delivered' AND created_at > now() - interval '24 hours'",
    )
    .bind(p)
    .fetch_one(&st.db)
    .await
    .map_err(ise)?;
    let states: serde_json::Map<String, Value> =
        by_state.into_iter().map(|(s, n)| (s, json!(n))).collect();
    Ok(Json(json!({
        "window_hours": 24,
        "by_state": states,
        "hourly": hourly.into_iter().map(|(h, n)| json!({ "hour": h, "messages": n })).collect::<Vec<_>>(),
        "delivery_latency_secs": { "p50": lat.0, "p95": lat.1 },
    })).into_response())
}

// ---------- credenciais dos fornecedores ----------

/// Metadados seguros de mostrar; a chave privada nunca sai da base depois de carregada.
async fn list_credentials(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> R {
    let (_, _, p) = project_access(&st, &h, id, Role::Admin).await?;
    let rows: Vec<(String, Value, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("SELECT provider, meta, updated_at FROM project_credentials WHERE project_id = $1 ORDER BY provider")
            .bind(p)
            .fetch_all(&st.db)
            .await
            .map_err(ise)?;
    Ok(Json(rows.into_iter().map(|(provider, meta, at)| json!({ "provider": provider, "meta": meta, "updated_at": at })).collect::<Vec<_>>()).into_response())
}

async fn put_credential(
    State(st): State<AppState>,
    h: HeaderMap,
    Path((id, kind)): Path<(Uuid, String)>,
    Json(b): Json<Value>,
) -> R {
    let (a, org, p) = project_access(&st, &h, id, Role::Admin).await?;
    let Some(key) = st.cfg.secret_key.as_ref() else {
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "o servidor não tem PUSH_SECRET_KEY: não guarda credenciais",
        ));
    };
    let invalid = |m: &str| err(StatusCode::UNPROCESSABLE_ENTITY, m);
    let (plain, meta) = match kind.as_str() {
        "fcm" => {
            let sa = b.get("service_account").unwrap_or(&b);
            let secret: crate::tenants::FcmSecret = serde_json::from_value(json!({
                "project_id": sa.get("project_id"), "client_email": sa.get("client_email"), "private_key": sa.get("private_key"),
            }))
            .map_err(|_| invalid("faltam project_id, client_email ou private_key da conta de serviço"))?;
            // Constrói-se já: uma chave ilegível diz-se aqui, e não à primeira mensagem.
            crate::tenants::build_fcm(&st, &secret)
                .map_err(|_| invalid("private_key ilegível (PEM RSA esperado)"))?;
            let meta =
                json!({ "project_id": secret.project_id, "client_email": secret.client_email });
            (serde_json::to_vec(&secret).map_err(ise)?, meta)
        }
        "apns" => {
            let secret: crate::tenants::ApnsSecret = serde_json::from_value(json!({
                "key_id": b.get("key_id"), "team_id": b.get("team_id"), "p8": b.get("p8"), "topic": b.get("topic"),
                "voip": b.get("voip").and_then(Value::as_bool).unwrap_or(false),
                "sandbox": b.get("sandbox").and_then(Value::as_bool).unwrap_or(false),
            }))
            .map_err(|_| invalid("faltam key_id, team_id, p8 ou topic"))?;
            crate::tenants::build_apns(&st, &secret)
                .map_err(|_| invalid("p8 ilegível (PEM EC esperado)"))?;
            let meta = json!({ "key_id": secret.key_id, "team_id": secret.team_id, "topic": secret.topic, "voip": secret.voip, "sandbox": secret.sandbox });
            (serde_json::to_vec(&secret).map_err(ise)?, meta)
        }
        _ => {
            return Err(err(
                StatusCode::NOT_FOUND,
                "fornecedor desconhecido (fcm|apns)",
            ))
        }
    };
    let sealed = crate::seal::seal(key, &crate::tenants::aad(p, &kind), &plain);
    sqlx::query(
        "INSERT INTO project_credentials (project_id, provider, sealed, meta) VALUES ($1,$2,$3,$4)
         ON CONFLICT (project_id, provider) DO UPDATE SET sealed = $3, meta = $4, updated_at = now()",
    )
    .bind(p)
    .bind(&kind)
    .bind(&sealed)
    .bind(&meta)
    .execute(&st.db)
    .await
    .map_err(ise)?;
    audit(&st.db, Some(org), Some(p), a, "credencial.carregada", &kind).await;
    Ok(Json(json!({ "provider": kind, "meta": meta })).into_response())
}

async fn delete_credential(
    State(st): State<AppState>,
    h: HeaderMap,
    Path((id, kind)): Path<(Uuid, String)>,
) -> R {
    let (a, org, p) = project_access(&st, &h, id, Role::Admin).await?;
    let n = sqlx::query("DELETE FROM project_credentials WHERE project_id = $1 AND provider = $2")
        .bind(p)
        .bind(&kind)
        .execute(&st.db)
        .await
        .map_err(ise)?
        .rows_affected();
    if n == 0 {
        return Err(err(StatusCode::NOT_FOUND, "credencial desconhecida"));
    }
    audit(&st.db, Some(org), Some(p), a, "credencial.removida", &kind).await;
    Ok(StatusCode::NO_CONTENT.into_response())
}
