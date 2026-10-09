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
    /// Chave de instalação (32 bytes) com que se cifram as credenciais dos fornecedores em repouso
    /// (`PUSH_SECRET_KEY`, 64 hex). Sem ela as credenciais por projecto não se guardam (503).
    pub secret_key: Option<[u8; 32]>,
    /// Limites por omissão de cada projecto (ajustáveis por projecto pelo operador): mensagens por segundo e por dia.
    pub default_rate_per_sec: u32,
    pub default_daily_quota: u64,
    /// `/metrics` só responde com este token (`Authorization: Bearer`); vazio = fechado.
    pub metrics_token: String,
    /// Destinos dos fornecedores. Fixos em produção: NUNCA vêm das credenciais que um inquilino carrega
    /// (senão um inquilino fazia o servidor chamar o URL que quisesse). Os testes apontam-nos a servidores de papel.
    pub fcm_base: String,
    pub fcm_token_uri: String,
    pub apns_base: String,
    pub apns_sandbox_base: String,
    /// Pasta com a consola web já construída (`console/dist`): se existir, o servidor serve-a em `/`.
    pub console_dir: Option<String>,
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
            secret_key: None,
            default_rate_per_sec: 500,
            default_daily_quota: 5_000_000,
            metrics_token: String::new(),
            fcm_base: "https://fcm.googleapis.com".into(),
            fcm_token_uri: "https://oauth2.googleapis.com/token".into(),
            apns_base: "https://api.push.apple.com".into(),
            apns_sandbox_base: "https://api.sandbox.push.apple.com".into(),
            console_dir: None,
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            admin_token: std::env::var("PUSH_ADMIN_TOKEN").unwrap_or_default(),
            console_dir: std::env::var("PUSH_CONSOLE_DIR")
                .ok()
                .filter(|d| !d.is_empty()),
            console_open_registration: std::env::var("PUSH_CONSOLE_OPEN_REGISTRATION")
                .is_ok_and(|v| v == "1"),
            ..Self::default()
        }
    }
}
