//! Barramento entre instâncias: «entrega esta mensagem, o aparelho está ligado a ti». Com `PUSH_REDIS_URL` usa o pub/sub do
//! Redis (um canal por instância); sem ele, o `LISTEN/NOTIFY` do Postgres (serve poucas instâncias: o NOTIFY serializa os
//! commits e deixa de escalar — medido: com 2 e 4 instâncias a capacidade descia em vez de subir).

use std::time::Duration;

use futures_util::StreamExt;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{dispatch, AppState};

pub enum Bus {
    Postgres,
    Redis {
        client: redis::Client,
        conn: Box<ConnectionManager>,
    },
}

pub fn channel(node: Uuid) -> String {
    format!("dpush:node:{node}")
}

impl Bus {
    pub async fn redis(url: &str) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(url)?;
        let conn = Box::new(ConnectionManager::new(client.clone()).await?);
        Ok(Self::Redis { client, conn })
    }

    /// Envia aos destinos (`<nó>:<id>,<id>…`, até 150 ids por aviso). Uma instrução por lote.
    pub async fn publish(&self, db: &PgPool, payloads: &[(Uuid, String)]) {
        match self {
            Self::Redis { conn, .. } => {
                let mut c = (**conn).clone();
                let mut pipe = redis::pipe();
                for (node, ids) in payloads {
                    pipe.cmd("PUBLISH").arg(channel(*node)).arg(ids).ignore();
                }
                if let Err(e) = pipe.query_async::<()>(&mut c).await {
                    tracing::warn!("barramento Redis: falhou o aviso entre instâncias ({e}); o worker reencaminha");
                }
            }
            Self::Postgres => {
                let texts: Vec<String> = payloads
                    .iter()
                    .map(|(n, ids)| format!("{n}:{ids}"))
                    .collect();
                let _ = sqlx::query("SELECT pg_notify($1, p) FROM unnest($2::text[]) AS p")
                    .bind(dispatch::CANAL)
                    .bind(&texts)
                    .execute(db)
                    .await;
            }
        }
    }
}

/// Escuta os avisos dirigidos a ESTA instância (Redis) e entrega. Reconecta sozinho; o `tick` do worker apanha o que se perder.
pub async fn run_redis_subscriber(st: AppState) {
    let crate::bus::Bus::Redis { client, .. } = &*st.bus else {
        return;
    };
    let client = client.clone();
    loop {
        let Ok(mut ps) = client.get_async_pubsub().await else {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };
        if ps.subscribe(channel(st.node_id)).await.is_err() {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        let mut stream = ps.on_message();
        while let Some(msg) = stream.next().await {
            let Ok(payload) = msg.get_payload::<String>() else {
                continue;
            };
            dispatch::handle_notice(&st, dispatch::parse_notice(&payload));
        }
    }
}
