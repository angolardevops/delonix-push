//! Várias instâncias do servidor sobre a mesma base: quem tem a ligação do aparelho é quem entrega.
mod common;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn a_instancia_que_recebe_o_pedido_nao_e_a_que_tem_a_ligacao(db: PgPool) {
    let a = app(db.clone(), |_| {}).await;
    let b = app(db, |_| {}).await;
    assert_ne!(a.st.node_id, b.st.node_id);
    let (_p, key) = a.project("meet").await;
    let (dev, secret) = a.device(&key, "android").await;

    // O aparelho liga-se à instância B; o pedido de envio entra pela A.
    let mut ws = b.ws(&secret).await;
    assert!(
        until(|| async {
            delonix_push::store::presence_node(&a.st.db, dev.parse().unwrap())
                .await
                .unwrap()
                == Some(b.st.node_id)
        })
        .await
    );
    let r: serde_json::Value = a
        .send(&key, json!({ "device_id": dev, "payload": {"via": "A"} }))
        .await
        .json()
        .await
        .unwrap();
    let id = r["message_ids"][0].as_str().unwrap().to_string();
    let m = ws.recv(5).await.expect("a B entregou o que a A recebeu");
    assert_eq!(m["id"], id);
    ws.ack(&id).await;
    assert!(until(|| async { a.state(&key, &id).await == "delivered" }).await);

    // Religa-se à A: a presença muda e o pedido que entra pela B chega pela A.
    drop(ws);
    let mut ws2 = a.ws(&secret).await;
    assert!(
        until(|| async {
            delonix_push::store::presence_node(&a.st.db, dev.parse().unwrap())
                .await
                .unwrap()
                == Some(a.st.node_id)
        })
        .await
    );
    let r: serde_json::Value = b
        .send(&key, json!({ "device_id": dev, "payload": {"via": "B"} }))
        .await
        .json()
        .await
        .unwrap();
    let id2 = r["message_ids"][0].as_str().unwrap().to_string();
    assert_eq!(
        ws2.recv(5).await.expect("a A entregou o que a B recebeu")["id"],
        id2
    );
}

#[sqlx::test]
async fn presenca_velha_de_uma_instancia_morta_nao_prende_a_mensagem(db: PgPool) {
    let a = app(db.clone(), |_| {}).await;
    let (_p, key) = a.project("meet").await;
    let (dev, secret) = a.device(&key, "android").await;
    // Uma «instância» que morreu: presença antiga de um nó que não existe.
    sqlx::query("INSERT INTO device_connections (device_id, node_id, seen_at) VALUES ($1::uuid, gen_random_uuid(), now() - interval '10 minutes')")
        .bind(&dev).execute(&a.st.db).await.unwrap();
    let r: serde_json::Value = a
        .send(&key, json!({ "device_id": dev, "payload": 1 }))
        .await
        .json()
        .await
        .unwrap();
    let id = r["message_ids"][0].as_str().unwrap().to_string();
    assert_eq!(
        a.state(&key, &id).await,
        "queued",
        "não se perde: espera pelo aparelho"
    );
    let mut ws = a.ws(&secret).await;
    assert_eq!(ws.recv(5).await.expect("entregue ao ligar")["id"], id);
}

#[sqlx::test]
async fn a_instancia_nao_entrega_o_que_nao_e_para_ela(db: PgPool) {
    // Duas instâncias a receber o MESMO pedido de entrega: a mensagem sai uma só vez.
    let a = app(db.clone(), |_| {}).await;
    let b = app(db, |_| {}).await;
    let (_p, key) = a.project("meet").await;
    let (dev, secret) = a.device(&key, "android").await;
    let mut ws = b.ws(&secret).await;
    assert!(
        until(|| async {
            delonix_push::store::presence_node(&a.st.db, dev.parse().unwrap())
                .await
                .unwrap()
                == Some(b.st.node_id)
        })
        .await
    );
    let r: serde_json::Value = a
        .send(&key, json!({ "device_id": dev, "payload": "uma-vez" }))
        .await
        .json()
        .await
        .unwrap();
    let id: uuid::Uuid = r["message_ids"][0].as_str().unwrap().parse().unwrap();
    // Pedidos repetidos de qualquer lado (como um NOTIFY duplicado ou o worker) não duplicam a entrega.
    delonix_push::dispatch::deliver(&a.st, id).await;
    delonix_push::dispatch::deliver(&b.st, id).await;
    assert_eq!(ws.recv(5).await.unwrap()["payload"], "uma-vez");
    assert!(ws.recv(1).await.is_none(), "só uma vez dentro do lease");
}

#[sqlx::test]
async fn com_redis_o_barramento_entre_instancias_entrega_em_lote(db: PgPool) {
    let a = app_redis(db.clone()).await;
    let b = app_redis(db).await;
    let (_p, key) = a.project("meet").await;
    let (dev, secret) = a.device(&key, "android").await;
    // O aparelho liga-se à B; os pedidos entram pela A: o aviso vai pelo Redis, não pelo NOTIFY do Postgres.
    let mut ws = b.ws(&secret).await;
    assert!(
        until(|| async {
            delonix_push::store::presence_node(&a.st.db, dev.parse().unwrap())
                .await
                .unwrap()
                == Some(b.st.node_id)
        })
        .await
    );
    // Dá tempo ao subscritor de B de se registar no canal antes de a A publicar.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let mut ids = vec![];
    for i in 0..20 {
        let r: serde_json::Value = a
            .send(&key, json!({"device_id": dev, "payload": {"n": i}}))
            .await
            .json()
            .await
            .unwrap();
        ids.push(r["message_ids"][0].as_str().unwrap().to_string());
    }
    let mut got = std::collections::HashSet::new();
    while got.len() < ids.len() {
        let m = ws
            .recv(5)
            .await
            .expect("a B entregou o que a A recebeu, via Redis");
        let id = m["id"].as_str().unwrap().to_string();
        ws.ack(&id).await;
        got.insert(id);
    }
    assert!(ids.iter().all(|i| got.contains(i)));
}
