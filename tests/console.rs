//! A API de gestão: contas, papéis, isolamento entre organizações, chaves, aparelhos, mensagens e estatísticas.
mod common;
use std::sync::Arc;

use common::*;
use serde_json::{json, Value};
use sqlx::PgPool;

async fn app_aberta(db: PgPool) -> App {
    app(db, |s| {
        let mut c = (*s.cfg).clone();
        c.console_open_registration = true;
        s.cfg = Arc::new(c);
    })
    .await
}

/// Regista e entra; devolve (id da conta, token da sessão).
async fn conta(a: &App, email: &str) -> (String, String) {
    let r = a
        .http
        .post(a.url("/console/v1/auth/register"))
        .json(&json!({"email": email, "password": "uma-senha-comprida"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201, "{email}");
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
    (
        l["account_id"].as_str().unwrap().into(),
        l["token"].as_str().unwrap().into(),
    )
}

async fn get(a: &App, tok: &str, p: &str) -> (u16, Value) {
    let r = a.http.get(a.url(p)).bearer_auth(tok).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}
async fn post(a: &App, tok: &str, p: &str, b: Value) -> (u16, Value) {
    let r = a
        .http
        .post(a.url(p))
        .bearer_auth(tok)
        .json(&b)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}
async fn del(a: &App, tok: &str, p: &str) -> u16 {
    a.http
        .delete(a.url(p))
        .bearer_auth(tok)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// Uma organização com um projecto; devolve (org, projecto, chave de servidor).
async fn org_e_projecto(a: &App, tok: &str, nome: &str) -> (String, String, String) {
    let (_, o) = post(a, tok, "/console/v1/orgs", json!({"name": nome})).await;
    let org = o["id"].as_str().unwrap().to_string();
    let (st, p) = post(
        a,
        tok,
        &format!("/console/v1/orgs/{org}/projects"),
        json!({"name": "app"}),
    )
    .await;
    assert_eq!(st, 201, "{p}");
    (
        org,
        p["id"].as_str().unwrap().into(),
        p["server_key"].as_str().unwrap().into(),
    )
}

#[sqlx::test]
async fn registo_fechado_por_omissao_e_aberto_por_configuracao(db: PgPool) {
    let fechada = app(db.clone(), |_| {}).await;
    let r = fechada
        .http
        .post(fechada.url("/console/v1/auth/register"))
        .json(&json!({"email": "a@b.co", "password": "uma-senha-comprida"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let aberta = app_aberta(db).await;
    conta(&aberta, "ana@exemplo.ao").await;
}

#[sqlx::test]
async fn contas_sessoes_e_validacao(db: PgPool) {
    let a = app_aberta(db).await;
    let (_, tok) = conta(&a, "Ana@Exemplo.ao").await; // o e-mail guarda-se em minúsculas
    let (st, me) = get(&a, &tok, "/console/v1/me").await;
    assert_eq!((st, me["email"].as_str()), (200, Some("ana@exemplo.ao")));
    // duplicada, senha curta, e-mail mau, senha errada
    let reg = |e: &str, p: &str| {
        let (e, p) = (e.to_string(), p.to_string());
        let a = &a;
        async move {
            a.http
                .post(a.url("/console/v1/auth/register"))
                .json(&json!({"email": e, "password": p}))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(reg("ana@exemplo.ao", "uma-senha-comprida").await, 409);
    assert_eq!(reg("outra@exemplo.ao", "curta").await, 422);
    assert_eq!(reg("sem-arroba", "uma-senha-comprida").await, 422);
    let l = a
        .http
        .post(a.url("/console/v1/auth/login"))
        .json(&json!({"email": "ana@exemplo.ao", "password": "errada-errada"}))
        .send()
        .await
        .unwrap();
    assert_eq!(l.status(), 401);
    let l = a
        .http
        .post(a.url("/console/v1/auth/login"))
        .json(&json!({"email": "nao-existe@exemplo.ao", "password": "uma-senha-comprida"}))
        .send()
        .await
        .unwrap();
    assert_eq!(l.status(), 401, "a mesma resposta para e-mail desconhecido");
    // a senha nunca fica em claro
    let h: String = sqlx::query_scalar("SELECT password_hash FROM accounts")
        .fetch_one(&a.st.db)
        .await
        .unwrap();
    assert!(h.starts_with("$argon2") && !h.contains("uma-senha-comprida"));
    // terminar a sessão
    assert_eq!(
        a.http
            .post(a.url("/console/v1/auth/logout"))
            .bearer_auth(&tok)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(get(&a, &tok, "/console/v1/me").await.0, 401);
    assert_eq!(get(&a, "dps_inventado", "/console/v1/me").await.0, 401);
}

#[sqlx::test]
async fn chaves_o_valor_so_aparece_uma_vez_e_revogar_corta(db: PgPool) {
    let a = app_aberta(db).await;
    let (_, tok) = conta(&a, "ana@exemplo.ao").await;
    let (_org, proj, key1) = org_e_projecto(&a, &tok, "Acme").await;
    // a chave inicial serve para a API de servidor
    assert_eq!(
        a.http
            .post(a.url("/v1/devices"))
            .bearer_auth(&key1)
            .json(&json!({"platform": "android"}))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );
    let (st, nova) = post(
        &a,
        &tok,
        &format!("/console/v1/projects/{proj}/keys"),
        json!({"label": "ci"}),
    )
    .await;
    assert_eq!(st, 201);
    let key2 = nova["key"].as_str().unwrap().to_string();
    assert!(key2.starts_with("dpk_"));
    let (_, lista) = get(&a, &tok, &format!("/console/v1/projects/{proj}/keys")).await;
    assert_eq!(lista.as_array().unwrap().len(), 2);
    assert!(
        !lista.to_string().contains(&key2) && !lista.to_string().contains(&key1),
        "a lista nunca mostra o valor"
    );
    // revogar a nova: a API de servidor recusa-a, a outra continua
    let kid = nova["id"].as_str().unwrap();
    assert_eq!(
        del(&a, &tok, &format!("/console/v1/projects/{proj}/keys/{kid}")).await,
        204
    );
    assert_eq!(
        a.http
            .post(a.url("/v1/devices"))
            .bearer_auth(&key2)
            .json(&json!({"platform": "android"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        a.http
            .post(a.url("/v1/devices"))
            .bearer_auth(&key1)
            .json(&json!({"platform": "android"}))
            .send()
            .await
            .unwrap()
            .status(),
        201
    );
    assert_eq!(
        del(&a, &tok, &format!("/console/v1/projects/{proj}/keys/{kid}")).await,
        404,
        "já revogada"
    );
}

#[sqlx::test]
async fn papeis_viewer_ve_developer_envia_admin_gere(db: PgPool) {
    let a = app_aberta(db).await;
    let (_, dono) = conta(&a, "dono@exemplo.ao").await;
    let (org, proj, key) = org_e_projecto(&a, &dono, "Acme").await;
    let (dev_id, _) = a.device(&key, "android").await;
    for (email, role) in [
        ("viewer@exemplo.ao", "viewer"),
        ("dev@exemplo.ao", "developer"),
        ("admin@exemplo.ao", "admin"),
    ] {
        let (id, _) = conta(&a, email).await;
        sqlx::query(
            "INSERT INTO memberships (account_id, org_id, role) VALUES ($1::uuid, $2::uuid, $3)",
        )
        .bind(&id)
        .bind(&org)
        .bind(role)
        .execute(&a.st.db)
        .await
        .unwrap();
    }
    let tok = |e: &str| {
        let e = e.to_string();
        let a = &a;
        async move {
            let l: Value = a
                .http
                .post(a.url("/console/v1/auth/login"))
                .json(&json!({"email": e, "password": "uma-senha-comprida"}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            l["token"].as_str().unwrap().to_string()
        }
    };
    let (viewer, devl, admin) = (
        tok("viewer@exemplo.ao").await,
        tok("dev@exemplo.ao").await,
        tok("admin@exemplo.ao").await,
    );
    let envio = json!({"device_id": dev_id, "payload": {"k": 1}});

    // viewer: vê, não mexe
    assert_eq!(
        get(&a, &viewer, &format!("/console/v1/projects/{proj}"))
            .await
            .0,
        200
    );
    assert_eq!(
        get(&a, &viewer, &format!("/console/v1/projects/{proj}/devices"))
            .await
            .0,
        200
    );
    assert_eq!(
        post(
            &a,
            &viewer,
            &format!("/console/v1/projects/{proj}/messages"),
            envio.clone()
        )
        .await
        .0,
        403
    );
    assert_eq!(
        post(
            &a,
            &viewer,
            &format!("/console/v1/projects/{proj}/keys"),
            json!({})
        )
        .await
        .0,
        403
    );
    assert_eq!(
        get(&a, &viewer, &format!("/console/v1/projects/{proj}/keys"))
            .await
            .0,
        403,
        "nem a lista de chaves"
    );
    assert_eq!(
        post(
            &a,
            &viewer,
            &format!("/console/v1/orgs/{org}/projects"),
            json!({"name": "x"})
        )
        .await
        .0,
        403
    );
    // developer: envia teste e revoga aparelhos, não gere chaves
    assert_eq!(
        post(
            &a,
            &devl,
            &format!("/console/v1/projects/{proj}/messages"),
            envio
        )
        .await
        .0,
        202
    );
    assert_eq!(
        post(
            &a,
            &devl,
            &format!("/console/v1/projects/{proj}/keys"),
            json!({})
        )
        .await
        .0,
        403
    );
    // admin: gere chaves e projectos
    assert_eq!(
        post(
            &a,
            &admin,
            &format!("/console/v1/projects/{proj}/keys"),
            json!({"label": "x"})
        )
        .await
        .0,
        201
    );
    assert_eq!(
        post(
            &a,
            &admin,
            &format!("/console/v1/orgs/{org}/projects"),
            json!({"name": "outro"})
        )
        .await
        .0,
        201
    );
    // a auditoria registou quem fez o quê
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action IN ('chave.criada','mensagem.teste','projecto.criado')").fetch_one(&a.st.db).await.unwrap();
    assert!(n >= 4, "{n}");
}

#[sqlx::test]
async fn organizacoes_nao_se_veem(db: PgPool) {
    let a = app_aberta(db).await;
    let (_, ana) = conta(&a, "ana@a.ao").await;
    let (_, bruno) = conta(&a, "bruno@b.ao").await;
    let (org_a, proj_a, key_a) = org_e_projecto(&a, &ana, "A").await;
    let (_org_b, proj_b, _) = org_e_projecto(&a, &bruno, "B").await;
    let (dev_a, _) = a.device(&key_a, "android").await;
    let msg: Value = a
        .send(&key_a, json!({"device_id": dev_a, "payload": 1}))
        .await
        .json()
        .await
        .unwrap();
    let _ = msg;

    // o Bruno não vê NADA do projecto da Ana: 404, igual a «não existe»
    for rota in ["", "/keys", "/devices", "/messages", "/stats"] {
        assert_eq!(
            get(&a, &bruno, &format!("/console/v1/projects/{proj_a}{rota}"))
                .await
                .0,
            404,
            "GET {rota}"
        );
    }
    assert_eq!(
        post(
            &a,
            &bruno,
            &format!("/console/v1/projects/{proj_a}/keys"),
            json!({})
        )
        .await
        .0,
        404
    );
    assert_eq!(
        post(
            &a,
            &bruno,
            &format!("/console/v1/projects/{proj_a}/messages"),
            json!({"device_id": dev_a, "payload": 1})
        )
        .await
        .0,
        404
    );
    assert_eq!(
        del(
            &a,
            &bruno,
            &format!("/console/v1/projects/{proj_a}/devices/{dev_a}")
        )
        .await,
        404
    );
    assert_eq!(
        get(&a, &bruno, &format!("/console/v1/orgs/{org_a}/projects"))
            .await
            .0,
        404
    );
    assert_eq!(
        post(
            &a,
            &bruno,
            &format!("/console/v1/orgs/{org_a}/projects"),
            json!({"name": "intruso"})
        )
        .await
        .0,
        404
    );
    // e as listas dele só trazem o que é dele
    let (_, orgs) = get(&a, &bruno, "/console/v1/orgs").await;
    assert_eq!(orgs.as_array().unwrap().len(), 1);
    // a chave do projecto B não envia para aparelhos do A (a API de servidor, já testada no core, confirma-se aqui)
    let (_, nova_b) = post(
        &a,
        &bruno,
        &format!("/console/v1/projects/{proj_b}/keys"),
        json!({}),
    )
    .await;
    let r = a
        .send(
            nova_b["key"].as_str().unwrap(),
            json!({"device_id": dev_a, "payload": 1}),
        )
        .await;
    assert_eq!(r.status(), 404);
}

#[sqlx::test]
async fn aparelhos_mensagens_e_estatisticas(db: PgPool) {
    let a = app_aberta(db).await;
    let (_, tok) = conta(&a, "ana@a.ao").await;
    let (_org, proj, key) = org_e_projecto(&a, &tok, "A").await;
    let (dev, secret) = a.device(&key, "android").await;
    let (_dev2, _) = a.device(&key, "ios").await;

    // o aparelho liga-se; a consola mostra-o ligado
    let mut ws = a.ws(&secret).await;
    assert!(
        until(|| async {
            let (_, l) = get(&a, &tok, &format!("/console/v1/projects/{proj}/devices")).await;
            l.as_array()
                .unwrap()
                .iter()
                .any(|d| d["id"] == dev && d["connected"] == true)
        })
        .await
    );
    let (_, resumo) = get(&a, &tok, &format!("/console/v1/projects/{proj}")).await;
    assert_eq!(
        (resumo["devices"].as_i64(), resumo["connected"].as_i64()),
        (Some(2), Some(1))
    );

    // enviar pela consola chega ao aparelho, e fica na lista de mensagens SEM o conteúdo
    let (st, env) = post(&a, &tok, &format!("/console/v1/projects/{proj}/messages"), json!({"device_id": dev, "payload": {"segredo": "conteudo-do-remetente"}, "priority": "high"})).await;
    assert_eq!(st, 202);
    let id = env["message_ids"][0].as_str().unwrap().to_string();
    let m = ws.recv(5).await.expect("entregue");
    assert_eq!(m["id"], id);
    ws.ack(&id).await;
    assert!(until(|| async { a.state(&key, &id).await == "delivered" }).await);
    let (_, msgs) = get(
        &a,
        &tok,
        &format!("/console/v1/projects/{proj}/messages?state=delivered"),
    )
    .await;
    assert_eq!(msgs.as_array().unwrap().len(), 1);
    assert!(
        !msgs.to_string().contains("conteudo-do-remetente"),
        "o conteúdo não se lista"
    );
    assert_eq!(
        get(
            &a,
            &tok,
            &format!("/console/v1/projects/{proj}/messages?state=inventado")
        )
        .await
        .0,
        422
    );
    let (_, vazio) = get(
        &a,
        &tok,
        &format!("/console/v1/projects/{proj}/messages?state=failed"),
    )
    .await;
    assert!(vazio.as_array().unwrap().is_empty());

    // estatísticas
    let (st, s) = get(&a, &tok, &format!("/console/v1/projects/{proj}/stats")).await;
    assert_eq!(st, 200);
    assert_eq!(s["by_state"]["delivered"], 1);
    assert!(
        s["hourly"].as_array().unwrap().len() == 1
            && s["delivery_latency_secs"]["p50"].as_f64().is_some()
    );

    // revogar o aparelho pela consola
    assert_eq!(
        del(
            &a,
            &tok,
            &format!("/console/v1/projects/{proj}/devices/{dev}")
        )
        .await,
        204
    );
    assert_eq!(
        del(
            &a,
            &tok,
            &format!("/console/v1/projects/{proj}/devices/{dev}")
        )
        .await,
        404
    );
}
