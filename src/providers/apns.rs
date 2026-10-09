//! APNs HTTP/2: `POST {base}/3/device/{token}`, autenticação por JWT ES256 (chave `.p8`).
//! `apns-push-type: voip` com tópico `<bundle>.voip` para chamadas (CallKit/PushKit).
//! **Nunca correu contra a Apple** (sem conta): medido só contra um servidor de papel.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;

use super::{classify, Outgoing, Provider, SendError};

pub struct Apns {
    http: reqwest::Client,
    base: String,
    key_id: String,
    team_id: String,
    key: EncodingKey,
    topic: String,
    voip: bool,
    jwt: Mutex<Option<(String, Instant)>>,
}

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    iat: u64,
}

impl Apns {
    /// `topic` é o *bundle id*; com `voip` acrescenta-se `.voip` e o tipo de push passa a `voip`.
    pub fn new(
        base: &str,
        key_id: &str,
        team_id: &str,
        p8_pem: &str,
        topic: &str,
        voip: bool,
    ) -> Result<Self, String> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|e| e.to_string())?,
            base: base.trim_end_matches('/').into(),
            key_id: key_id.into(),
            team_id: team_id.into(),
            key: EncodingKey::from_ec_pem(p8_pem.as_bytes()).map_err(|e| e.to_string())?,
            topic: topic.into(),
            voip,
            jwt: Mutex::new(None),
        })
    }

    fn bearer(&self) -> Result<String, SendError> {
        if let Some((t, at)) = self.jwt.lock().unwrap().as_ref() {
            // A Apple recusa tokens com mais de 1 h e pede que não se renovem em menos de 20 min.
            if at.elapsed() < Duration::from_secs(2400) {
                return Ok(t.clone());
            }
        }
        let mut h = Header::new(Algorithm::ES256);
        h.kid = Some(self.key_id.clone());
        let iat = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let t = jsonwebtoken::encode(
            &h,
            &Claims {
                iss: &self.team_id,
                iat,
            },
            &self.key,
        )
        .map_err(|e| SendError::Permanent(format!("jwt: {e}")))?;
        *self.jwt.lock().unwrap() = Some((t.clone(), Instant::now()));
        Ok(t)
    }
}

#[async_trait]
impl Provider for Apns {
    async fn send(&self, token: &str, m: &Outgoing) -> Result<(), SendError> {
        let jwt = self.bearer()?;
        let expiration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + m.ttl_secs.max(0);
        let topic = if self.voip {
            format!("{}.voip", self.topic)
        } else {
            self.topic.clone()
        };
        let mut req = self
            .http
            .post(format!("{}/3/device/{}", self.base, token))
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", topic)
            .header(
                "apns-push-type",
                if self.voip { "voip" } else { "background" },
            )
            .header("apns-priority", if m.high_priority { "10" } else { "5" })
            .header("apns-expiration", expiration.to_string())
            .header("apns-id", m.id.to_string());
        if let Some(c) = &m.collapse_key {
            req = req.header("apns-collapse-id", c);
        }
        let body = serde_json::json!({ "aps": { "content-available": 1 }, "dpush_id": m.id.to_string(), "payload": m.payload });
        let r = req
            .json(&body)
            .send()
            .await
            .map_err(|e| SendError::Transient(e.to_string()))?;
        let status = r.status().as_u16();
        if status == 200 {
            return Ok(());
        }
        let text = r.text().await.unwrap_or_default();
        // 410 Unregistered; 400 BadDeviceToken também significa «este token não serve».
        if status == 410 || text.contains("BadDeviceToken") || text.contains("Unregistered") {
            return Err(SendError::InvalidToken);
        }
        Err(classify(status, &text))
    }
}
