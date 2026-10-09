//! Fornecedores por projecto: cada projecto usa as SUAS credenciais FCM/APNs (cifradas na base), nunca as de outro.
//! Os fornecedores constroem-se uma vez e reaproveitam-se enquanto a credencial não mudar (`updated_at`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::providers::{apns::Apns, fcm::Fcm};
use crate::{seal, AppState, Provider};

type Entry = (DateTime<Utc>, Arc<dyn Provider>);

#[derive(Default)]
pub struct Cache(Mutex<HashMap<(Uuid, String), Entry>>);

/// O que se guarda cifrado para o FCM (campos da conta de serviço do Firebase).
#[derive(Serialize, Deserialize)]
pub struct FcmSecret {
    pub project_id: String,
    pub client_email: String,
    pub private_key: String,
}

/// O que se guarda cifrado para o APNs (chave `.p8` da Apple).
#[derive(Serialize, Deserialize)]
pub struct ApnsSecret {
    pub key_id: String,
    pub team_id: String,
    pub p8: String,
    pub topic: String,
    pub voip: bool,
    pub sandbox: bool,
}

pub fn aad(project: Uuid, provider: &str) -> String {
    format!("{project}:{provider}")
}

pub fn build_fcm(st: &AppState, s: &FcmSecret) -> Result<Arc<dyn Provider>, String> {
    Ok(Arc::new(Fcm::new(
        &st.cfg.fcm_base,
        &s.project_id,
        &s.client_email,
        &s.private_key,
        &st.cfg.fcm_token_uri,
    )?))
}

pub fn build_apns(st: &AppState, s: &ApnsSecret) -> Result<Arc<dyn Provider>, String> {
    let base = if s.sandbox {
        &st.cfg.apns_sandbox_base
    } else {
        &st.cfg.apns_base
    };
    Ok(Arc::new(Apns::new(
        base, &s.key_id, &s.team_id, &s.p8, &s.topic, s.voip,
    )?))
}

/// O fornecedor do projecto: o das credenciais dele, ou (só se não tem nenhumas) o global configurado no processo.
pub async fn provider_for(st: &AppState, project: Uuid, kind: &str) -> Option<Arc<dyn Provider>> {
    let row: Option<(Vec<u8>, DateTime<Utc>)> =
        sqlx::query_as("SELECT sealed, updated_at FROM project_credentials WHERE project_id = $1 AND provider = $2")
            .bind(project)
            .bind(kind)
            .fetch_optional(&st.db)
            .await
            .ok()
            .flatten();
    let Some((sealed, updated)) = row else {
        return if kind == "fcm" {
            st.fcm.clone()
        } else {
            st.apns.clone()
        };
    };
    let key = (project, kind.to_string());
    if let Some((at, p)) = st.provider_cache.0.lock().unwrap().get(&key) {
        if *at == updated {
            return Some(p.clone());
        }
    }
    let k = st.cfg.secret_key.as_ref()?;
    let plain = seal::open(k, &aad(project, kind), &sealed)?;
    let built = match kind {
        "fcm" => build_fcm(st, &serde_json::from_slice(&plain).ok()?),
        _ => build_apns(st, &serde_json::from_slice(&plain).ok()?),
    }
    .ok()?;
    st.provider_cache
        .0
        .lock()
        .unwrap()
        .insert(key, (updated, built.clone()));
    Some(built)
}
