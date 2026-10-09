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
    /// Máximo de envios (`POST /v1/messages`) a decorrer ao mesmo tempo nesta instância. Acima disto responde 503 com
    /// `Retry-After` em vez de aceitar e deixar a fila crescer (medido: sem isto, a 2× a capacidade a latência passava de
    /// 10 s e a memória multiplicava-se). 0 = recusa tudo (só testes).
    pub max_inflight_sends: usize,
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
            max_inflight_sends: 256,
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// A configuração a partir de um «dicionário» de variáveis (as de ambiente, ou as de um teste).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let d = Self::default();
        fn num<T: std::str::FromStr>(get: &dyn Fn(&str) -> Option<String>, k: &str) -> Option<T> {
            get(k).and_then(|v| v.trim().parse().ok())
        }
        Self {
            admin_token: get("PUSH_ADMIN_TOKEN").unwrap_or_default(),
            console_dir: get("PUSH_CONSOLE_DIR").filter(|v| !v.is_empty()),
            console_open_registration: get("PUSH_CONSOLE_OPEN_REGISTRATION")
                .is_some_and(|v| v == "1"),
            secret_key: get("PUSH_SECRET_KEY")
                .and_then(|h| hex::decode(h.trim()).ok())
                .and_then(|b| <[u8; 32]>::try_from(b).ok()),
            metrics_token: get("PUSH_METRICS_TOKEN").unwrap_or_default(),
            default_rate_per_sec: num(&get, "PUSH_DEFAULT_RATE_PER_SEC")
                .unwrap_or(d.default_rate_per_sec),
            default_daily_quota: num(&get, "PUSH_DEFAULT_DAILY_QUOTA")
                .unwrap_or(d.default_daily_quota),
            max_inflight_sends: num(&get, "PUSH_MAX_INFLIGHT_SENDS")
                .unwrap_or(d.max_inflight_sends),
            ..d
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> Config {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn reads_every_documented_variable() {
        let c = cfg(&[
            ("PUSH_ADMIN_TOKEN", "adm"),
            ("PUSH_METRICS_TOKEN", "met"),
            ("PUSH_SECRET_KEY", &"ab".repeat(32)),
            ("PUSH_CONSOLE_OPEN_REGISTRATION", "1"),
            ("PUSH_CONSOLE_DIR", "/srv/console"),
            ("PUSH_DEFAULT_RATE_PER_SEC", "77"),
            ("PUSH_DEFAULT_DAILY_QUOTA", "1234"),
            ("PUSH_MAX_INFLIGHT_SENDS", "9"),
        ]);
        assert_eq!(c.admin_token, "adm");
        assert_eq!(c.metrics_token, "met");
        assert_eq!(c.secret_key, Some([0xab; 32]));
        assert!(c.console_open_registration);
        assert_eq!(c.console_dir.as_deref(), Some("/srv/console"));
        assert_eq!(
            (
                c.default_rate_per_sec,
                c.default_daily_quota,
                c.max_inflight_sends
            ),
            (77, 1234, 9)
        );
    }

    #[test]
    fn safe_defaults_and_bad_values() {
        let c = cfg(&[]);
        assert!(
            c.secret_key.is_none()
                && c.metrics_token.is_empty()
                && !c.console_open_registration
                && c.admin_token.is_empty()
        );
        // uma chave que não tem 64 hex NÃO vale (melhor sem chave do que uma chave curta)
        assert!(cfg(&[("PUSH_SECRET_KEY", "abcd")]).secret_key.is_none());
        assert!(cfg(&[("PUSH_SECRET_KEY", &"zz".repeat(32))])
            .secret_key
            .is_none());
        assert_eq!(
            cfg(&[("PUSH_DEFAULT_RATE_PER_SEC", "muitos")]).default_rate_per_sec,
            Config::default().default_rate_per_sec
        );
        assert!(
            !cfg(&[("PUSH_CONSOLE_OPEN_REGISTRATION", "true")]).console_open_registration,
            "só «1» abre o registo"
        );
    }
}
