use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    /// Segredo do plano de administração (criar projectos). Vazio = rotas de administração fechadas.
    pub admin_token: String,
    pub max_payload_bytes: usize,
    pub default_ttl_secs: i64,
    pub max_ttl_secs: i64,
    /// Quanto tempo se espera pelo *ack* antes de reenviar.
    pub ack_timeout: Duration,
    pub max_attempts: i32,
    pub worker_interval: Duration,
    pub max_fanout: i64,
    /// Qualquer pessoa pode criar conta na consola (`PUSH_CONSOLE_OPEN_REGISTRATION=1`). Desligado por omissão.
    pub console_open_registration: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            admin_token: String::new(),
            max_payload_bytes: 4096,
            default_ttl_secs: 28 * 24 * 3600,
            max_ttl_secs: 28 * 24 * 3600,
            ack_timeout: Duration::from_secs(30),
            max_attempts: 20,
            worker_interval: Duration::from_secs(1),
            max_fanout: 1000,
            console_open_registration: false,
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            admin_token: std::env::var("PUSH_ADMIN_TOKEN").unwrap_or_default(),
            console_open_registration: std::env::var("PUSH_CONSOLE_OPEN_REGISTRATION")
                .is_ok_and(|v| v == "1"),
            ..Self::default()
        }
    }
}
