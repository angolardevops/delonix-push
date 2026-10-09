//! Limites por projecto (por segundo e por dia), partilhados entre instâncias com Redis, e as métricas.
mod common;
use std::sync::Arc;

use common::*;
use delonix_push::limits::Limiter;
use serde_json::{json, Value};
use sqlx::PgPool;

async fn com_limite(db: PgPool, rate: u32) -> App {
    app(db, move |s| {
        let mut c = (*s.cfg).clone();
        c.default_rate_per_sec = rate;
        c.metrics_token = "metricas-secretas".into();
        s.cfg = Arc::new(c);
    })
    .await
}

async fn tres_aparelhos_no_topico(a: &App, key: &str, topic: &str, n: usize) -> Vec<String> {
    let mut ids = vec![];
    for _ in 0..n {
        let (d, _) = a.device(key, "android").await;
        a.http
            .put(a.url(&format!("/v1/devices/{d}/topics/{topic}")))
            .bearer_auth(key)
            .send()
            .await
            .unwrap();
        ids.push(d);
    }
    ids
}

#[sqlx::test]
async fn por_segundo_um_topico_conta_um_por_aparelho(db: PgPool) {
    let a = com_limite(db, 2).await;
    let (_p, key) = a.project("meet").await;
    tres_aparelhos_no_topico(&a, &key, "sala", 3).await;
    // 3 aparelhos custam 3 > 2 por segundo: recusado de uma vez, com Retry-After
    let r = a.send(&key, json!({"topic": "sala", "payload": 1})).await;
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers()["retry-after"], "1");
    let corpo: Value = r.json().await.unwrap();
    assert!(corpo["error"].as_str().unwrap().contains("por segundo"));
    // nada ficou na fila
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM messages")
        .fetch_one(&a.st.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test]
async fn quota_diaria_e_so_o_operador_a_altera(db: PgPool) {
    let a = com_limite(db, 1000).await;
    let (pid, key) = a.project("meet").await;
    let (dev, _) = a.device(&key, "android").await;
    let envia = || a.send(&key, json!({"device_id": dev, "payload": 1}));

    // o inquilino não consegue mexer nos limites (só o operador, com o segredo de administração)
    let sem = a
        .http
        .put(a.url(&format!("/admin/v1/projects/{pid}/limits")))
        .json(&json!({"daily_quota": 2}))
        .send()
        .await
        .unwrap();
    assert_eq!(sem.status(), 401);
    let com = a
        .http
        .put(a.url(&format!("/admin/v1/projects/{pid}/limits")))
        .header("x-admin-token", ADMIN)
        .json(&json!({"daily_quota": 2}))
        .send()
        .await
        .unwrap();
    assert_eq!(com.status(), 204);
    let mau = a
        .http
        .put(a.url(&format!("/admin/v1/projects/{pid}/limits")))
        .header("x-admin-token", ADMIN)
        .json(&json!({"daily_quota": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(mau.status(), 422);

    assert_eq!(envia().await.status(), 202);
    assert_eq!(envia().await.status(), 202);
    let r = envia().await;
    assert_eq!(r.status(), 429);
    let segs: u64 = r.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=86_400).contains(&segs));
    assert!(r.json::<Value>().await.unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("quota diária"));
}

#[sqlx::test]
async fn um_projecto_nao_gasta_a_quota_do_outro(db: PgPool) {
    let a = com_limite(db, 1000).await;
    let (pa, ka) = a.project("A").await;
    let (_pb, kb) = a.project("B").await;
    let _ = pa;
    let (da, _) = a.device(&ka, "android").await;
    let (db_, _) = a.device(&kb, "android").await;
    a.http
        .put(a.url(&format!("/admin/v1/projects/{pa}/limits")))
        .header("x-admin-token", ADMIN)
        .json(&json!({"daily_quota": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        a.send(&ka, json!({"device_id": da, "payload": 1}))
            .await
            .status(),
        202
    );
    assert_eq!(
        a.send(&ka, json!({"device_id": da, "payload": 1}))
            .await
            .status(),
        429
    );
    for _ in 0..3 {
        assert_eq!(
            a.send(&kb, json!({"device_id": db_, "payload": 1}))
                .await
                .status(),
            202,
            "o B não é afectado pelo A"
        );
    }
}

#[sqlx::test]
async fn com_redis_o_limite_e_partilhado_entre_instancias(db: PgPool) {
    let url = std::env::var("TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:56379".into());
    let redis = Arc::new(Limiter::redis(&url).await.expect("Redis de teste"));
    let (r1, r2) = (redis.clone(), redis);
    let a = app(db.clone(), move |s| s.limiter = r1).await;
    let b = app(db, move |s| s.limiter = r2).await;
    let (pid, key) = a.project("meet").await;
    tres_aparelhos_no_topico(&a, &key, "sala", 2).await;
    a.http
        .put(a.url(&format!("/admin/v1/projects/{pid}/limits")))
        .header("x-admin-token", ADMIN)
        .json(&json!({"daily_quota": 3}))
        .send()
        .await
        .unwrap();
    // cada difusão custa 2: a primeira (pela A) cabe, a segunda (pela B) já passa de 3 — a conta é de todos
    assert_eq!(
        a.send(&key, json!({"topic": "sala", "payload": 1}))
            .await
            .status(),
        202
    );
    assert_eq!(
        b.send(&key, json!({"topic": "sala", "payload": 1}))
            .await
            .status(),
        429
    );
}

#[sqlx::test]
async fn metricas_fechadas_sem_token_e_contam_o_que_aconteceu(db: PgPool) {
    let a = com_limite(db, 1000).await;
    let (_p, key) = a.project("meet").await;
    let (dev, secret) = a.device(&key, "android").await;
    assert_eq!(
        a.http.get(a.url("/metrics")).send().await.unwrap().status(),
        403
    );
    assert_eq!(
        a.http
            .get(a.url("/metrics"))
            .bearer_auth("errado")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );

    let mut ws = a.ws(&secret).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let r: Value = a
        .send(&key, json!({"device_id": dev, "payload": 1}))
        .await
        .json()
        .await
        .unwrap();
    let id = r["message_ids"][0].as_str().unwrap().to_string();
    ws.recv(5).await.unwrap();
    ws.ack(&id).await;
    assert!(until(|| async { a.state(&key, &id).await == "delivered" }).await);

    let txt = a
        .http
        .get(a.url("/metrics"))
        .bearer_auth("metricas-secretas")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let valor = |nome: &str| -> u64 {
        txt.lines()
            .find(|l| l.starts_with(nome) && !l.starts_with('#'))
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    };
    assert_eq!(valor("dpush_messages_enqueued_total"), 1);
    assert_eq!(valor("dpush_messages_acked_total"), 1);
    assert_eq!(valor("dpush_ws_connections"), 1);
    assert!(txt.contains("# TYPE dpush_ws_connections gauge"));
}

#[sqlx::test]
async fn sobrecarga_recusa_cedo_com_503_e_conta_nas_metricas(db: PgPool) {
    // Zero licenças: toda a gente é «a mais». Em carga real o limite é `max_inflight_sends`.
    let a = app(db, |s| {
        let mut c = (*s.cfg).clone();
        c.metrics_token = "m".into();
        c.max_inflight_sends = 0;
        s.cfg = Arc::new(c);
        s.send_permits = Arc::new(tokio::sync::Semaphore::new(0));
    })
    .await;
    let (_p, key) = a.project("meet").await;
    let (dev, _) = a.device(&key, "android").await;
    let r = a.send(&key, json!({"device_id": dev, "payload": 1})).await;
    assert_eq!(r.status(), 503);
    assert_eq!(r.headers()["retry-after"], "1");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM messages")
        .fetch_one(&a.st.db)
        .await
        .unwrap();
    assert_eq!(n, 0, "recusado antes de tocar na base");
    let txt = a
        .http
        .get(a.url("/metrics"))
        .bearer_auth("m")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(txt.contains("dpush_sends_shed_total 1"), "{txt}");
}
