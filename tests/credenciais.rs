//! Credenciais FCM/APNs por projecto: cifradas na base, nunca devolvidas, e cada projecto usa só as suas.
mod common;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use common::*;
use serde_json::{json, Value};
use sqlx::PgPool;

type Vistos = Arc<Mutex<Vec<(String, Value)>>>;

/// Um «FCM» de papel que regista a quem (o `iss` do JWT que trocou) e o que recebeu.
async fn fcm_de_papel() -> (String, Vistos) {
    let vistos: Vistos = Arc::default();
    let app = Router::new()
        .route(
            "/token",
            post(|State(v): State<Vistos>, body: String| async move {
                let jwt = body
                    .split('&')
                    .find_map(|p| p.strip_prefix("assertion="))
                    .unwrap_or("")
                    .to_string();
                // O payload do JWT (sem verificar a assinatura: aqui só interessa quem se identificou).
                let payload = jwt.split('.').nth(1).unwrap_or("");
                let raw = base64::Engine::decode(
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                    payload,
                )
                .unwrap_or_default();
                let iss = serde_json::from_slice::<Value>(&raw)
                    .ok()
                    .and_then(|j| j["iss"].as_str().map(String::from))
                    .unwrap_or_default();
                v.lock()
                    .unwrap()
                    .push((format!("TOKEN iss={iss}"), Value::Null));
                Json(json!({ "access_token": format!("at-de-{iss}") }))
            }),
        )
        .route(
            "/v1/projects/{p}/messages:send",
            post(
                |State(v): State<Vistos>, h: HeaderMap, Json(b): Json<Value>| async move {
                    let auth = h
                        .get("authorization")
                        .and_then(|x| x.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    v.lock().unwrap().push((format!("SEND {auth}"), b));
                    Json(json!({ "name": "ok" }))
                },
            ),
        )
        .with_state(vistos.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (base, vistos)
}

async fn app_com(db: PgPool, fcm_base: &str) -> App {
    let fcm_base = fcm_base.to_string();
    app(db, move |s| {
        let mut c = (*s.cfg).clone();
        c.console_open_registration = true;
        c.secret_key = Some([9u8; 32]);
        c.fcm_base = fcm_base.clone();
        c.fcm_token_uri = format!("{fcm_base}/token");
        s.cfg = Arc::new(c);
    })
    .await
}

async fn conta(a: &App, email: &str) -> String {
    a.http
        .post(a.url("/console/v1/auth/register"))
        .json(&json!({"email": email, "password": "uma-senha-comprida"}))
        .send()
        .await
        .unwrap();
    let l: Value = a
        .http
        .post(a.url("/console/v1/auth/login"))
        .json(&json!({"email": email, "password": "uma-senha-comprida"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    l["token"].as_str().unwrap().into()
}

async fn projecto(a: &App, tok: &str, nome: &str) -> (String, String) {
    let o: Value = a
        .http
        .post(a.url("/console/v1/orgs"))
        .bearer_auth(tok)
        .json(&json!({"name": nome}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let p: Value = a
        .http
        .post(a.url(&format!(
            "/console/v1/orgs/{}/projects",
            o["id"].as_str().unwrap()
        )))
        .bearer_auth(tok)
        .json(&json!({"name": "app"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (
        p["id"].as_str().unwrap().into(),
        p["server_key"].as_str().unwrap().into(),
    )
}

fn conta_servico(email: &str) -> Value {
    json!({ "service_account": {
        "project_id": "fb-proj", "client_email": email,
        "private_key": std::fs::read_to_string("tests/fixtures/fcm_test_rsa.pem").unwrap(),
    }})
}

#[sqlx::test]
async fn cada_projecto_usa_as_suas_credenciais_e_a_chave_nunca_volta(db: PgPool) {
    let (base, vistos) = fcm_de_papel().await;
    let a = app_com(db, &base).await;
    let tok = conta(&a, "ana@a.ao").await;
    let (pa, ka) = projecto(&a, &tok, "A").await;
    let (_pb, kb) = projecto(&a, &tok, "B").await;

    let r = a
        .http
        .put(a.url(&format!("/console/v1/projects/{pa}/credentials/fcm")))
        .bearer_auth(&tok)
        .json(&conta_servico("sa-do-a@fb.iam"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let corpo = r.text().await.unwrap();
    assert!(
        !corpo.contains("BEGIN"),
        "a resposta nunca leva a chave privada"
    );

    // na base está cifrada: nem o PEM nem o e-mail em claro
    let sealed: Vec<u8> = sqlx::query_scalar("SELECT sealed FROM project_credentials")
        .fetch_one(&a.st.db)
        .await
        .unwrap();
    let txt = String::from_utf8_lossy(&sealed);
    assert!(!txt.contains("BEGIN") && !txt.contains("sa-do-a@fb.iam"));
    let lista: Value = a
        .http
        .get(a.url(&format!("/console/v1/projects/{pa}/credentials")))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lista[0]["meta"]["client_email"], "sa-do-a@fb.iam");
    assert!(!lista.to_string().contains("BEGIN"));

    // um aparelho de cada projecto com token FCM
    for (key, tag) in [(&ka, "tok-a"), (&kb, "tok-b")] {
        let (_d, secret) = a.device(key, "android").await;
        let r = a
            .http
            .put(a.url("/v1/device/provider"))
            .bearer_auth(&secret)
            .json(&json!({"provider": "fcm", "token": tag}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 204);
    }
    let da: Value = a
        .http
        .get(a.url(&format!("/console/v1/projects/{pa}/devices")))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let dev_a = da[0]["id"].as_str().unwrap().to_string();
    let r = a
        .send(&ka, json!({"device_id": dev_a, "payload": {"k": 1}}))
        .await;
    assert_eq!(r.status(), 202);
    let id: Value = r.json().await.unwrap();
    assert_eq!(
        a.state(&ka, id["message_ids"][0].as_str().unwrap()).await,
        "accepted"
    );
    {
        let v = vistos.lock().unwrap();
        assert!(
            v.iter().any(|(k, _)| k == "TOKEN iss=sa-do-a@fb.iam"),
            "{v:?}"
        );
        let envio = v.iter().find(|(k, _)| k.starts_with("SEND")).unwrap();
        assert_eq!(envio.0, "SEND Bearer at-de-sa-do-a@fb.iam");
        assert_eq!(envio.1["message"]["token"], "tok-a");
    }

    // o projecto B não tem credenciais (nem global): a mensagem espera na fila, não usa as do A
    let db_: Value = a
        .http
        .get(a.url(&format!("/console/v1/projects/{_pb}/devices")))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r = a
        .send(&kb, json!({"device_id": db_[0]["id"], "payload": 1}))
        .await;
    let idb: Value = r.json().await.unwrap();
    assert_eq!(
        a.state(&kb, idb["message_ids"][0].as_str().unwrap()).await,
        "queued"
    );
    assert_eq!(
        vistos
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with("SEND"))
            .count(),
        1,
        "nada saiu para o B"
    );
}

#[sqlx::test]
async fn validacao_papeis_e_remocao(db: PgPool) {
    let (base, _) = fcm_de_papel().await;
    let a = app_com(db.clone(), &base).await;
    let tok = conta(&a, "ana@a.ao").await;
    let (p, _) = projecto(&a, &tok, "A").await;
    let put = |kind: &str, body: Value| {
        let (a, tok, p, kind) = (&a, tok.clone(), p.clone(), kind.to_string());
        async move {
            a.http
                .put(a.url(&format!("/console/v1/projects/{p}/credentials/{kind}")))
                .bearer_auth(tok)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(put("fcm", json!({"service_account": {"project_id": "x", "client_email": "e", "private_key": "isto-nao-e-pem"}})).await, 422);
    assert_eq!(
        put("fcm", json!({"service_account": {"project_id": "x"}})).await,
        422
    );
    assert_eq!(
        put(
            "apns",
            json!({"key_id": "K", "team_id": "T", "p8": "nao-e-pem", "topic": "b"})
        )
        .await,
        422
    );
    assert_eq!(put("apns", json!({"key_id": "K", "team_id": "T", "p8": std::fs::read_to_string("tests/fixtures/apns_test_p8.pem").unwrap(), "topic": "ao.exemplo", "voip": true})).await, 200);
    assert_eq!(put("webpush", json!({})).await, 404);
    let r = a
        .http
        .delete(a.url(&format!("/console/v1/projects/{p}/credentials/apns")))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    let r = a
        .http
        .delete(a.url(&format!("/console/v1/projects/{p}/credentials/apns")))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // sem PUSH_SECRET_KEY o servidor recusa guardar (503), em vez de guardar em claro
    let sem = app(db, |s| {
        let mut c = (*s.cfg).clone();
        c.console_open_registration = true;
        s.cfg = Arc::new(c);
    })
    .await;
    let t2 = conta(&sem, "bruno@b.ao").await;
    let (p2, _) = projecto(&sem, &t2, "B").await;
    let r = sem
        .http
        .put(sem.url(&format!("/console/v1/projects/{p2}/credentials/fcm")))
        .bearer_auth(&t2)
        .json(&conta_servico("x@y.z"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    // e um viewer não vê nem carrega credenciais
    let (viewer_id, _) = {
        sem.http
            .post(sem.url("/console/v1/auth/register"))
            .json(&json!({"email": "v@b.ao", "password": "uma-senha-comprida"}))
            .send()
            .await
            .unwrap();
        let id: String = sqlx::query_scalar("SELECT id::text FROM accounts WHERE email = 'v@b.ao'")
            .fetch_one(&sem.st.db)
            .await
            .unwrap();
        (id, 0)
    };
    sqlx::query("INSERT INTO memberships (account_id, org_id, role) SELECT $1::uuid, org_id, 'viewer' FROM projects WHERE id = $2::uuid").bind(&viewer_id).bind(&p2).execute(&sem.st.db).await.unwrap();
    let l: Value = sem
        .http
        .post(sem.url("/console/v1/auth/login"))
        .json(&json!({"email": "v@b.ao", "password": "uma-senha-comprida"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let vt = l["token"].as_str().unwrap();
    assert_eq!(
        sem.http
            .get(sem.url(&format!("/console/v1/projects/{p2}/credentials")))
            .bearer_auth(vt)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
}
