mod common;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn entrega_com_ack_e_fila_offline(db: PgPool) {
    let a = app(db, |_| {}).await;
    let (_p, key) = a.project("meet").await;
    let (dev, secret) = a.device(&key, "android").await;

    // Aparelho offline: a mensagem fica na fila.
    let r = a
        .send(
            &key,
            json!({ "device_id": dev, "payload": { "tipo": "chamada", "n": 1 } }),
        )
        .await;
    assert_eq!(r.status(), 202);
    let id = r.json::<serde_json::Value>().await.unwrap()["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(a.state(&key, &id).await, "queued");

    // Liga-se: recebe o que esperava, por ordem; sem ack fica «sent».
    let mut ws = a.ws(&secret).await;
    let m = ws.recv(5).await.expect("mensagem à espera");
    assert_eq!(m["id"], id);
    assert_eq!(m["payload"]["tipo"], "chamada");
    assert!(until(|| async { a.state(&key, &id).await == "sent" }).await);
    ws.ack(&id).await;
    assert!(until(|| async { a.state(&key, &id).await == "delivered" }).await);
}

#[sqlx::test]
async fn sem_ack_reenvia_e_ligacao_viva_recebe_na_hora(db: PgPool) {
    let a = app(db, |_| {}).await;
    let (_p, key) = a.project("meet").await;
    let (dev, secret) = a.device(&key, "android").await;
    let mut ws = a.ws(&secret).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let r = a
        .send(
            &key,
            json!({ "device_id": dev, "payload": {"x": 1}, "priority": "high" }),
        )
        .await;
    let id = r.json::<serde_json::Value>().await.unwrap()["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    let m1 = ws.recv(5).await.expect("entrega imediata");
    assert_eq!(m1["id"], id);
    assert_eq!(m1["priority"], "high");
    // Sem ack, o worker reenvia depois do ack_timeout (400 ms nos testes).
    delonix_push::dispatch::tick(&a.st).await; // ainda dentro do lease: nada
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    delonix_push::dispatch::tick(&a.st).await;
    let m2 = ws.recv(5).await.expect("reenvio");
    assert_eq!(m2["id"], id, "mesmo id: a app deduplica");
    ws.ack(&id).await;
    assert!(until(|| async { a.state(&key, &id).await == "delivered" }).await);
}

#[sqlx::test]
async fn ttl_expira_e_collapse_substitui(db: PgPool) {
    let a = app(db, |_| {}).await;
    let (_p, key) = a.project("meet").await;
    let (dev, secret) = a.device(&key, "android").await;
    let id = |r: serde_json::Value| r["message_ids"][0].as_str().unwrap().to_string();

    let curta = id(a
        .send(
            &key,
            json!({ "device_id": dev, "payload": 1, "ttl_secs": 0 }),
        )
        .await
        .json()
        .await
        .unwrap());
    delonix_push::dispatch::tick(&a.st).await;
    assert_eq!(a.state(&key, &curta).await, "expired");

    let m1 = id(a
        .send(
            &key,
            json!({ "device_id": dev, "payload": "v1", "collapse_key": "estado" }),
        )
        .await
        .json()
        .await
        .unwrap());
    let m2 = id(a
        .send(
            &key,
            json!({ "device_id": dev, "payload": "v2", "collapse_key": "estado" }),
        )
        .await
        .json()
        .await
        .unwrap());
    assert_eq!(
        a.state(&key, &m1).await,
        "expired",
        "a antiga foi substituída"
    );
    let mut ws = a.ws(&secret).await;
    let got = ws.recv(5).await.unwrap();
    assert_eq!(got["id"], m2);
    assert_eq!(got["payload"], "v2");
    assert!(ws.recv(1).await.is_none(), "só chega a última");
}

#[sqlx::test]
async fn idempotencia_nao_duplica(db: PgPool) {
    let a = app(db, |_| {}).await;
    let (_p, key) = a.project("meet").await;
    let (dev, _s) = a.device(&key, "android").await;
    let body = json!({ "device_id": dev, "payload": {"a":1}, "idempotency_key": "pedido-1" });
    let r1: serde_json::Value = a.send(&key, body.clone()).await.json().await.unwrap();
    let r2: serde_json::Value = a.send(&key, body).await.json().await.unwrap();
    assert_eq!(r1["message_ids"], r2["message_ids"]);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM messages")
        .fetch_one(&a.st.db)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn topicos_difundem_e_respeitam_o_limite(db: PgPool) {
    let a = app(db, |_| {}).await;
    let (_p, key) = a.project("meet").await;
    let (d1, s1) = a.device(&key, "android").await;
    let (d2, s2) = a.device(&key, "ios").await;
    let (_d3, s3) = a.device(&key, "android").await;
    for d in [&d1, &d2] {
        let r = a
            .http
            .put(a.url(&format!("/v1/devices/{d}/topics/sala-1")))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 204);
    }
    let r: serde_json::Value = a
        .send(&key, json!({ "topic": "sala-1", "payload": "oi" }))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(r["accepted"], 2);
    for s in [&s1, &s2] {
        let mut ws = a.ws(s).await;
        assert_eq!(ws.recv(5).await.unwrap()["payload"], "oi");
    }
    let mut ws3 = a.ws(&s3).await;
    assert!(
        ws3.recv(1).await.is_none(),
        "quem não subscreveu não recebe"
    );
    assert_eq!(
        a.send(&key, json!({ "topic": "a b", "payload": 1 }))
            .await
            .status(),
        422
    );
    assert_eq!(a.send(&key, json!({ "payload": 1 })).await.status(), 422);
}

#[sqlx::test]
async fn limites_do_pedido(db: PgPool) {
    let a = app(db, |_| {}).await;
    let (_p, key) = a.project("meet").await;
    let (dev, _s) = a.device(&key, "android").await;
    let grande = "x".repeat(5000);
    assert_eq!(
        a.send(&key, json!({ "device_id": dev, "payload": grande }))
            .await
            .status(),
        413
    );
    assert_eq!(
        a.send(
            &key,
            json!({ "device_id": dev, "payload": 1, "priority": "urgente" })
        )
        .await
        .status(),
        422
    );
    assert_eq!(
        a.send(
            &key,
            json!({ "device_id": dev, "payload": 1, "ttl_secs": 99999999 })
        )
        .await
        .status(),
        422
    );
    assert_eq!(
        a.send("dpk_falsa", json!({ "device_id": dev, "payload": 1 }))
            .await
            .status(),
        401
    );
}

#[sqlx::test]
async fn projectos_nao_se_veem(db: PgPool) {
    let a = app(db, |_| {}).await;
    let (_pa, ka) = a.project("A").await;
    let (_pb, kb) = a.project("B").await;
    let (da, sa) = a.device(&ka, "android").await;
    let (db_, sb) = a.device(&kb, "android").await;

    // B não envia, não subscreve, não revoga e não lê estado de um aparelho/mensagem de A.
    assert_eq!(
        a.send(&kb, json!({ "device_id": da, "payload": 1 }))
            .await
            .status(),
        404
    );
    let r = a
        .http
        .put(a.url(&format!("/v1/devices/{da}/topics/t")))
        .bearer_auth(&kb)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = a
        .http
        .delete(a.url(&format!("/v1/devices/{da}")))
        .bearer_auth(&kb)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let id = a
        .send(&ka, json!({ "device_id": da, "payload": "segredo-de-A" }))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap()["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        a.http
            .get(a.url(&format!("/v1/messages/{id}")))
            .bearer_auth(&kb)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );

    // Um tópico com o mesmo nome em B não apanha A.
    a.http
        .put(a.url(&format!("/v1/devices/{da}/topics/geral")))
        .bearer_auth(&ka)
        .send()
        .await
        .unwrap();
    a.http
        .put(a.url(&format!("/v1/devices/{db_}/topics/geral")))
        .bearer_auth(&kb)
        .send()
        .await
        .unwrap();
    a.send(&kb, json!({ "topic": "geral", "payload": "so-para-B" }))
        .await;
    let mut wa = a.ws(&sa).await;
    let m = wa.recv(5).await.unwrap();
    assert_eq!(m["payload"], "segredo-de-A");
    assert!(wa.recv(1).await.is_none(), "A não recebe o que B difundiu");
    let mut wb = a.ws(&sb).await;
    assert_eq!(wb.recv(5).await.unwrap()["payload"], "so-para-B");

    // O aparelho de A não confirma mensagens de B.
    let idb = a
        .send(&kb, json!({ "device_id": db_, "payload": 2 }))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap()["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    wa.ack(&idb).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_ne!(a.state(&kb, &idb).await, "delivered");
}

#[sqlx::test]
async fn revogar_aparelho_corta_ligacao_nova_e_fila(db: PgPool) {
    let a = app(db, |_| {}).await;
    let (_p, key) = a.project("meet").await;
    let (dev, secret) = a.device(&key, "android").await;
    let id = a
        .send(&key, json!({ "device_id": dev, "payload": 1 }))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap()["message_ids"][0]
        .as_str()
        .unwrap()
        .to_string();
    let r = a
        .http
        .delete(a.url(&format!("/v1/devices/{dev}")))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert_eq!(a.state(&key, &id).await, "expired");
    let mut req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
        format!("ws://{}/v1/connect", a.base),
    )
    .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {secret}").parse().unwrap());
    assert!(
        tokio_tungstenite::connect_async(req).await.is_err(),
        "segredo revogado não liga"
    );
    assert_eq!(
        a.send(&key, json!({ "device_id": dev, "payload": 1 }))
            .await
            .status(),
        404
    );
}

#[sqlx::test]
async fn administracao_fechada_sem_segredo(db: PgPool) {
    let a = app(db, |_| {}).await;
    let r = a
        .http
        .post(a.url("/admin/v1/projects"))
        .json(&json!({ "name": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = a
        .http
        .post(a.url("/admin/v1/projects"))
        .header("x-admin-token", "errado")
        .json(&json!({ "name": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}
