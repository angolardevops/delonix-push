//! Os adaptadores FCM/APNs contra servidores de papel que verificam o que a Google/Apple verificariam:
//! a assinatura do JWT, os cabeçalhos e o corpo. NÃO é prova contra os serviços reais.
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use delonix_push::providers::{apns::Apns, fcm::Fcm, Outgoing, Provider, SendError};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, HeaderMap, Value)>>>);

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

fn out() -> Outgoing {
    Outgoing {
        id: Uuid::new_v4(),
        payload: json!({"k":"v"}),
        high_priority: true,
        ttl_secs: 60,
        collapse_key: Some("c1".into()),
    }
}

#[tokio::test]
async fn fcm_troca_jwt_assinado_e_manda_so_dados() {
    let seen = Seen::default();
    let app = Router::new()
        .route(
            "/token",
            post(
                |State(s): State<Seen>, h: HeaderMap, body: String| async move {
                    s.0.lock().unwrap().push(("token".into(), h, json!(body)));
                    Json(json!({ "access_token": "at-123", "expires_in": 3600 }))
                },
            ),
        )
        .route(
            "/v1/projects/proj-x/messages:send",
            post(
                |State(s): State<Seen>, h: HeaderMap, Json(b): Json<Value>| async move {
                    s.0.lock().unwrap().push(("send".into(), h, b));
                    Json(json!({ "name": "projects/proj-x/messages/1" }))
                },
            ),
        )
        .with_state(seen.clone());
    let base = serve(app).await;
    let pem = std::fs::read_to_string("tests/fixtures/fcm_test_rsa.pem").unwrap();
    let f = Fcm::new(
        &base,
        "proj-x",
        "sa@proj-x.iam",
        &pem,
        &format!("{base}/token"),
    )
    .unwrap();
    f.send("tok-1", &out()).await.unwrap();
    f.send("tok-1", &out()).await.unwrap(); // o token de acesso é reaproveitado

    let s = seen.0.lock().unwrap();
    assert_eq!(
        s.iter().filter(|x| x.0 == "token").count(),
        1,
        "uma troca de token só"
    );
    // O JWT que a Google receberia verifica com a chave pública e traz o âmbito certo.
    let form = s[0].2.as_str().unwrap();
    let jwt = form
        .split('&')
        .find_map(|p| p.strip_prefix("assertion="))
        .unwrap();
    let mut v = Validation::new(Algorithm::RS256);
    v.set_audience(&[format!("{base}/token")]);
    let pubk = std::fs::read("tests/fixtures/fcm_test_rsa.pub.pem").unwrap();
    let claims = jsonwebtoken::decode::<Value>(jwt, &DecodingKey::from_rsa_pem(&pubk).unwrap(), &v)
        .unwrap()
        .claims;
    assert_eq!(claims["iss"], "sa@proj-x.iam");
    assert_eq!(
        claims["scope"],
        "https://www.googleapis.com/auth/firebase.messaging"
    );
    let m = s.iter().find(|x| x.0 == "send").unwrap();
    assert_eq!(m.1["authorization"], "Bearer at-123");
    assert_eq!(m.2["message"]["token"], "tok-1");
    assert_eq!(m.2["message"]["android"]["priority"], "HIGH");
    assert_eq!(m.2["message"]["android"]["collapse_key"], "c1");
    assert!(
        m.2["message"].get("notification").is_none(),
        "só dados: a app decide o que mostrar"
    );
}

#[tokio::test]
async fn fcm_classifica_erros() {
    for (status, body, esperado) in [
        (404, r#"{"error":{"status":"UNREGISTERED"}}"#, "token"),
        (503, "indisponivel", "transitorio"),
        (400, "payload mau", "permanente"),
    ] {
        let app = Router::new()
            .route(
                "/token",
                post(|| async { Json(json!({ "access_token": "a" })) }),
            )
            .route(
                "/v1/projects/p/messages:send",
                post(move || async move { (StatusCode::from_u16(status).unwrap(), body) }),
            );
        let base = serve(app).await;
        let pem = std::fs::read_to_string("tests/fixtures/fcm_test_rsa.pem").unwrap();
        let f = Fcm::new(&base, "p", "e", &pem, &format!("{base}/token")).unwrap();
        let e = f.send("t", &out()).await.unwrap_err();
        let got = match e {
            SendError::InvalidToken => "token",
            SendError::Transient(_) => "transitorio",
            SendError::Permanent(_) => "permanente",
        };
        assert_eq!(got, esperado, "estado {status}");
    }
}

#[tokio::test]
async fn apns_assina_es256_e_usa_o_topico_voip() {
    let seen = Seen::default();
    let app = Router::new()
        .route(
            "/3/device/{tok}",
            post(
                |State(s): State<Seen>,
                 axum::extract::Path(t): axum::extract::Path<String>,
                 h: HeaderMap,
                 Json(b): Json<Value>| async move {
                    s.0.lock().unwrap().push((t, h, b));
                    StatusCode::OK
                },
            ),
        )
        .with_state(seen.clone());
    let base = serve(app).await;
    let pem = std::fs::read_to_string("tests/fixtures/apns_test_p8.pem").unwrap();
    let a = Apns::new(
        &base,
        "KEY123",
        "TEAM99",
        &pem,
        "ao.ngolacloud.delonixphone",
        true,
    )
    .unwrap();
    a.send("devtoken", &out()).await.unwrap();

    let s = seen.0.lock().unwrap();
    let (tok, h, b) = &s[0];
    assert_eq!(tok, "devtoken");
    assert_eq!(h["apns-topic"], "ao.ngolacloud.delonixphone.voip");
    assert_eq!(h["apns-push-type"], "voip");
    assert_eq!(h["apns-priority"], "10");
    assert_eq!(h["apns-collapse-id"], "c1");
    assert_eq!(b["payload"]["k"], "v");
    let jwt = h["authorization"]
        .to_str()
        .unwrap()
        .strip_prefix("bearer ")
        .unwrap();
    assert_eq!(
        jsonwebtoken::decode_header(jwt).unwrap().kid.as_deref(),
        Some("KEY123")
    );
    let mut v = Validation::new(Algorithm::ES256);
    v.required_spec_claims.clear();
    let pubk = std::fs::read("tests/fixtures/apns_test_p8.pub.pem").unwrap();
    let c = jsonwebtoken::decode::<Value>(jwt, &DecodingKey::from_ec_pem(&pubk).unwrap(), &v)
        .unwrap()
        .claims;
    assert_eq!(c["iss"], "TEAM99");
}

#[tokio::test]
async fn apns_410_e_token_morto() {
    let app = Router::new().route(
        "/3/device/{tok}",
        post(|| async { (StatusCode::GONE, r#"{"reason":"Unregistered"}"#) }),
    );
    let base = serve(app).await;
    let pem = std::fs::read_to_string("tests/fixtures/apns_test_p8.pem").unwrap();
    let a = Apns::new(&base, "K", "T", &pem, "b", false).unwrap();
    assert!(matches!(
        a.send("x", &out()).await,
        Err(SendError::InvalidToken)
    ));
}
