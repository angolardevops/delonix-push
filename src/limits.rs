//! Limites por projecto: mensagens por segundo (janela de 1 s) e por dia (UTC). Com Redis são partilhados por todas as
//! instâncias; sem Redis cada instância conta sozinha (serve uma instância só e os testes).
//! É uma janela fixa, não um *token bucket*: pode deixar passar até ao dobro do limite à volta da fronteira de um segundo.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use redis::aio::ConnectionManager;
use uuid::Uuid;

pub enum Limiter {
    Redis(Box<ConnectionManager>),
    Memory(Mutex<HashMap<String, u64>>),
}

/// Porque é que se recusou, e quando vale a pena voltar a tentar.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    RatePerSec { retry_after_secs: u64 },
    DailyQuota { retry_after_secs: u64 },
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

impl Limiter {
    pub fn memory() -> Self {
        Self::Memory(Mutex::default())
    }

    pub async fn redis(url: &str) -> Result<Self, redis::RedisError> {
        let c = redis::Client::open(url)?;
        Ok(Self::Redis(Box::new(ConnectionManager::new(c).await?)))
    }

    /// Soma `cost` aos contadores e diz se cabe. Uma recusa NÃO devolve o que somou (uma tentativa recusada continua
    /// a contar para o segundo corrente: protege de quem insiste).
    pub async fn check(
        &self,
        project: Uuid,
        cost: u64,
        per_sec: u32,
        daily: u64,
    ) -> Result<(), Refused> {
        let now = now_secs();
        let day = now / 86_400;
        let k_sec = format!("dpush:rate:{project}:{now}");
        let k_day = format!("dpush:quota:{project}:{day}");
        let (n_sec, n_day) = self.incr(&k_sec, &k_day, cost).await;
        if n_sec > u64::from(per_sec) {
            return Err(Refused::RatePerSec {
                retry_after_secs: 1,
            });
        }
        if n_day > daily {
            return Err(Refused::DailyQuota {
                retry_after_secs: 86_400 - now % 86_400,
            });
        }
        Ok(())
    }

    async fn incr(&self, k_sec: &str, k_day: &str, cost: u64) -> (u64, u64) {
        match self {
            Self::Memory(m) => {
                let mut m = m.lock().unwrap();
                // Arruma os contadores de segundos antigos para não crescer sem fim.
                if m.len() > 10_000 {
                    m.retain(|k, _| k.starts_with("dpush:quota:"));
                }
                let a = m.entry(k_sec.to_string()).or_insert(0);
                *a += cost;
                let a = *a;
                let b = m.entry(k_day.to_string()).or_insert(0);
                *b += cost;
                (a, *b)
            }
            Self::Redis(c) => {
                let mut c = (**c).clone();
                let r: redis::RedisResult<(u64, i64, u64, i64)> = redis::pipe()
                    .atomic()
                    .cmd("INCRBY")
                    .arg(k_sec)
                    .arg(cost)
                    .cmd("EXPIRE")
                    .arg(k_sec)
                    .arg(5)
                    .cmd("INCRBY")
                    .arg(k_day)
                    .arg(cost)
                    .cmd("EXPIRE")
                    .arg(k_day)
                    .arg(2 * 86_400)
                    .query_async(&mut c)
                    .await;
                // Se o Redis falhar, deixa passar (disponibilidade acima do limite): regista-se.
                match r {
                    Ok((a, _, b, _)) => (a, b),
                    Err(e) => {
                        tracing::warn!("limites: Redis indisponível, a deixar passar: {e}");
                        (0, 0)
                    }
                }
            }
        }
    }
}
