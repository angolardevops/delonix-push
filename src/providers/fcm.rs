//! FCM HTTP v1: `POST {base}/v1/projects/{project}/messages:send`, mensagem só de dados.
//! O token de acesso OAuth2 vem de uma conta de serviço (JWT RS256 trocado em `token_uri`).
//! **Nunca correu contra o FCM real** (sem conta): está medido só contra um servidor de papel.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use serde_json::json;

use super::{classify, Outgoing, Provider, SendError};

pub struct Fcm {
    http: reqwest::Client,
    base: String,
    project: String,
    client_email: String,
    key: EncodingKey,
    token_uri: String,
    cache: Mutex<Option<(String, Instant)>>,
}

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

impl Fcm {
    pub fn new(
        base: &str,
        project: &str,
        client_email: &str,
        private_key_pem: &str,
        token_uri: &str,
    ) -> Result<Self, String> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|e| e.to_string())?,
            base: base.trim_end_matches('/').into(),
            project: project.into(),
            client_email: client_email.into(),
            key: EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
                .map_err(|e| e.to_string())?,
            token_uri: token_uri.into(),
            cache: Mutex::new(None),
        })
    }

    async fn access_token(&self) -> Result<String, SendError> {
        if let Some((t, at)) = self.cache.lock().unwrap().as_ref() {
            if at.elapsed() < Duration::from_secs(3000) {
                return Ok(t.clone());
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = Claims {
            iss: &self.client_email,
            scope: "https://www.googleapis.com/auth/firebase.messaging",
            aud: &self.token_uri,
            iat: now,
            exp: now + 3600,
        };
        let jwt = jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &self.key)
            .map_err(|e| SendError::Permanent(format!("jwt: {e}")))?;
        let r = self
            .http
            .post(&self.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .send()
            .await
            .map_err(|e| SendError::Transient(e.to_string()))?;
        let status = r.status().as_u16();
        let body = r.text().await.unwrap_or_default();
        if status != 200 {
            return Err(classify(status, &body));
        }
        let v: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| SendError::Transient(e.to_string()))?;
        let t = v["access_token"]
            .as_str()
            .ok_or_else(|| SendError::Permanent("sem access_token".into()))?
            .to_string();
        *self.cache.lock().unwrap() = Some((t.clone(), Instant::now()));
        Ok(t)
    }
}

#[async_trait]
impl Provider for Fcm {
    async fn send(&self, token: &str, m: &Outgoing) -> Result<(), SendError> {
        let access = self.access_token().await?;
        // O FCM só aceita strings em `data`.
        let data = json!({ "dpush_id": m.id.to_string(), "payload": m.payload.to_string() });
        let mut android = json!({
            "priority": if m.high_priority { "HIGH" } else { "NORMAL" },
            "ttl": format!("{}s", m.ttl_secs.max(0)),
        });
        if let Some(c) = &m.collapse_key {
            android["collapse_key"] = json!(c);
        }
        let body = json!({ "message": { "token": token, "data": data, "android": android } });
        let r = self
            .http
            .post(format!(
                "{}/v1/projects/{}/messages:send",
                self.base, self.project
            ))
            .bearer_auth(access)
            .json(&body)
            .send()
            .await
            .map_err(|e| SendError::Transient(e.to_string()))?;
        let status = r.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let text = r.text().await.unwrap_or_default();
        // O FCM devolve 404 UNREGISTERED para tokens mortos.
        if status == 404 || text.contains("UNREGISTERED") {
            return Err(SendError::InvalidToken);
        }
        Err(classify(status, &text))
    }
}
