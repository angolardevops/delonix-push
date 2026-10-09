mod common;
use std::sync::Arc;

use common::*;
use delonix_push::providers::SendError;
use serde_json::json;
use sqlx::PgPool;

async fn registar(a: &App, secret: &str, provider: &str, token: &str) {
    let r = a
        .http
        .put(a.url("/v1/device/provider"))
        .bearer_auth(secret)
        .json(&json!({ "provider": provider, "token": token }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
}

#[sqlx::test]
async fn sem_ligacao_usa_o_fornecedor_certo(db: PgPool) {
    let fcm = Arc::new(Fake::default());
    let apns = Arc::new(Fake::default());
    let (f, p) = (fcm.clone(), apns.clone());
    let a = app(db, move |s| {
        s.fcm = Some(f);
        s.apns = Some(p);
    })
    .await;
    let (_p, key) = a.project("meet").await;
    let (da, sa) = a.device(&key, "android").await;
    let (di, si) = a.device(&key, "ios").await;
    registar(&a, &sa, "fcm", "tok-android").await;
    registar(&a, &si, "apns", "tok-ios").await;

    let ida = a
        .send(
            &key,
            json!({ "device_id": da, "payload": {"k":1}, "priority": "high", "collapse_key": "c" }),
        )
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap()["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    let idi = a
        .send(&key, json!({ "device_id": di, "payload": {"k":2} }))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap()["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(a.state(&key, &ida).await, "accepted");
    assert_eq!(a.state(&key, &idi).await, "accepted");
    let fc = fcm.calls.lock().unwrap();
    assert_eq!(fc.len(), 1);
    assert_eq!(fc[0].0, "tok-android");
    assert!(fc[0].1.high_priority && fc[0].1.collapse_key.as_deref() == Some("c"));
    assert_eq!(apns.calls.lock().unwrap()[0].0, "tok-ios");
}

#[sqlx::test]
async fn ligacao_viva_ganha_ao_fornecedor(db: PgPool) {
    let fcm = Arc::new(Fake::default());
    let f = fcm.clone();
    let a = app(db, move |s| s.fcm = Some(f)).await;
    let (_p, key) = a.project("meet").await;
    let (d, s) = a.device(&key, "android").await;
    registar(&a, &s, "fcm", "tok").await;
    let mut ws = a.ws(&s).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    a.send(&key, json!({ "device_id": d, "payload": 1 })).await;
    assert!(ws.recv(5).await.is_some());
    assert!(
        fcm.calls.lock().unwrap().is_empty(),
        "não gasta o FCM quando há ligação própria"
    );
}

#[sqlx::test]
async fn token_morto_esquece_se_e_a_mensagem_espera_pela_ligacao(db: PgPool) {
    let fcm = Arc::new(Fake::default());
    fcm.script
        .lock()
        .unwrap()
        .push(Err(SendError::InvalidToken));
    let f = fcm.clone();
    let a = app(db, move |s| s.fcm = Some(f)).await;
    let (_p, key) = a.project("meet").await;
    let (d, s) = a.device(&key, "android").await;
    registar(&a, &s, "fcm", "tok-morto").await;
    let id = a
        .send(&key, json!({ "device_id": d, "payload": "ainda-importa" }))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap()["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    let (prov, tok): (String, Option<String>) =
        sqlx::query_as("SELECT provider, provider_token FROM devices WHERE id = $1::uuid")
            .bind(&d)
            .fetch_one(&a.st.db)
            .await
            .unwrap();
    assert_eq!((prov.as_str(), tok), ("direct", None));
    assert_eq!(a.state(&key, &id).await, "queued");
    let mut ws = a.ws(&s).await;
    assert_eq!(ws.recv(5).await.unwrap()["payload"], "ainda-importa");
}

#[sqlx::test]
async fn falha_transitoria_tenta_de_novo_e_permanente_falha(db: PgPool) {
    let fcm = Arc::new(Fake::default());
    fcm.script
        .lock()
        .unwrap()
        .push(Err(SendError::Transient("503".into())));
    let f = fcm.clone();
    let a = app(db, move |s| s.fcm = Some(f)).await;
    let (_p, key) = a.project("meet").await;
    let (d, s) = a.device(&key, "android").await;
    registar(&a, &s, "fcm", "tok").await;
    let id = a
        .send(&key, json!({ "device_id": d, "payload": 1 }))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap()["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(a.state(&key, &id).await, "queued");
    // Recuo: antecipa-se a próxima tentativa e o worker volta a tentar.
    sqlx::query("UPDATE messages SET next_attempt_at = now() WHERE id = $1::uuid")
        .bind(&id)
        .execute(&a.st.db)
        .await
        .unwrap();
    delonix_push::dispatch::tick(&a.st).await;
    assert_eq!(a.state(&key, &id).await, "accepted");

    fcm.script
        .lock()
        .unwrap()
        .push(Err(SendError::Permanent("400 payload".into())));
    let id2 = a
        .send(&key, json!({ "device_id": d, "payload": 2 }))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap()["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(a.state(&key, &id2).await, "failed");
}

#[sqlx::test]
async fn fornecedor_errado_para_a_plataforma_e_recusado(db: PgPool) {
    let a = app(db, |_| {}).await;
    let (_p, key) = a.project("meet").await;
    let (_d, s) = a.device(&key, "ios").await;
    let r = a
        .http
        .put(a.url("/v1/device/provider"))
        .bearer_auth(&s)
        .json(&json!({ "provider": "fcm", "token": "t" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
}
