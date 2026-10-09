#![allow(dead_code)]
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use delonix_push::providers::{Outgoing, SendError};
use delonix_push::{api, config::Config, AppState, Provider};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as T;

pub const ADMIN: &str = "admin-de-teste";

pub struct App {
    pub st: AppState,
    pub base: String,
    pub http: reqwest::Client,
}

pub async fn app(db: PgPool, tweak: impl FnOnce(&mut AppState)) -> App {
    let cfg = Config {
        admin_token: ADMIN.into(),
        ack_timeout: Duration::from_millis(400),
        ..Config::default()
    };
    let mut st = AppState::new(db, cfg);
    tweak(&mut st);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("127.0.0.1:{}", l.local_addr().unwrap().port());
    let router = api::router(st.clone());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    App {
        st,
        base,
        http: reqwest::Client::new(),
    }
}

impl App {
    pub fn url(&self, p: &str) -> String {
        format!("http://{}{p}", self.base)
    }

    /// Cria um projecto; devolve (id, chave de servidor).
    pub async fn project(&self, name: &str) -> (String, String) {
        let r: Value = self
            .http
            .post(self.url("/admin/v1/projects"))
            .header("x-admin-token", ADMIN)
            .json(&json!({ "name": name }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        (
            r["project_id"].as_str().unwrap().into(),
            r["server_key"].as_str().unwrap().into(),
        )
    }

    pub async fn device(&self, key: &str, platform: &str) -> (String, String) {
        let r: Value = self
            .http
            .post(self.url("/v1/devices"))
            .bearer_auth(key)
            .json(&json!({ "platform": platform }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        (
            r["device_id"].as_str().unwrap().into(),
            r["device_secret"].as_str().unwrap().into(),
        )
    }

    pub async fn send(&self, key: &str, body: Value) -> reqwest::Response {
        self.http
            .post(self.url("/v1/messages"))
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    pub async fn state(&self, key: &str, id: &str) -> String {
        let r: Value = self
            .http
            .get(self.url(&format!("/v1/messages/{id}")))
            .bearer_auth(key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        r["state"].as_str().unwrap_or("?").into()
    }

    pub async fn ws(&self, secret: &str) -> Ws {
        let mut req = format!("ws://{}/v1/connect", self.base)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {secret}").parse().unwrap());
        let (s, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        Ws(s)
    }
}

pub struct Ws(
    pub tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
);

impl Ws {
    pub async fn recv(&mut self, secs: u64) -> Option<Value> {
        loop {
            match tokio::time::timeout(Duration::from_secs(secs), self.0.next()).await {
                Ok(Some(Ok(T::Text(t)))) => return Some(serde_json::from_str(&t).unwrap()),
                Ok(Some(Ok(_))) => continue,
                _ => return None,
            }
        }
    }
    pub async fn ack(&mut self, id: &str) {
        self.0
            .send(T::Text(json!({ "type": "ack", "id": id }).to_string()))
            .await
            .unwrap();
    }
}

pub async fn until<F: std::future::Future<Output = bool>>(mut f: impl FnMut() -> F) -> bool {
    for _ in 0..60 {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Fornecedor de papel: regista as chamadas e responde com o próximo resultado programado.
#[derive(Default)]
pub struct Fake {
    pub calls: Mutex<Vec<(String, Outgoing)>>,
    pub script: Mutex<Vec<Result<(), SendError>>>,
}
#[async_trait]
impl Provider for Fake {
    async fn send(&self, token: &str, m: &Outgoing) -> Result<(), SendError> {
        self.calls.lock().unwrap().push((token.into(), m.clone()));
        let mut s = self.script.lock().unwrap();
        if s.is_empty() {
            Ok(())
        } else {
            s.remove(0)
        }
    }
}
