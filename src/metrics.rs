//! Métricas no formato de texto do Prometheus. Contadores por instância (o Prometheus soma-as).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub struct Metrics {
    pub enqueued: AtomicU64,
    pub rate_limited: AtomicU64,
    pub acked: AtomicU64,
    pub provider_accepted: AtomicU64,
    pub provider_failed: AtomicU64,
    pub ws_opened: AtomicU64,
    pub ws_closed: AtomicU64,
}

impl Metrics {
    pub fn inc(c: &AtomicU64) {
        c.fetch_add(1, Relaxed);
    }

    pub fn render(&self, connections_now: usize) -> String {
        let l = |c: &AtomicU64| c.load(Relaxed);
        let mut s = String::new();
        let mut counter = |name: &str, help: &str, v: u64| {
            s.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}\n"
            ));
        };
        counter(
            "dpush_messages_enqueued_total",
            "Mensagens aceites na fila.",
            l(&self.enqueued),
        );
        counter(
            "dpush_messages_rate_limited_total",
            "Pedidos de envio recusados pelos limites (429).",
            l(&self.rate_limited),
        );
        counter(
            "dpush_messages_acked_total",
            "Mensagens confirmadas (ack) pelo aparelho.",
            l(&self.acked),
        );
        counter(
            "dpush_provider_accepted_total",
            "Mensagens aceites pelo FCM/APNs.",
            l(&self.provider_accepted),
        );
        counter(
            "dpush_provider_failed_total",
            "Mensagens falhadas no FCM/APNs.",
            l(&self.provider_failed),
        );
        counter(
            "dpush_ws_opened_total",
            "Ligações WebSocket abertas.",
            l(&self.ws_opened),
        );
        counter(
            "dpush_ws_closed_total",
            "Ligações WebSocket fechadas.",
            l(&self.ws_closed),
        );
        s.push_str(&format!(
            "# HELP dpush_ws_connections Ligações WebSocket vivas nesta instância.\n# TYPE dpush_ws_connections gauge\ndpush_ws_connections {connections_now}\n"
        ));
        s
    }
}
