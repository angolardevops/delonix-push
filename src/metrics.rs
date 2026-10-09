//! Métricas no formato de texto do Prometheus. Contadores por instância (o Prometheus soma-as).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

/// Limites superiores (µs) dos baldes dos histogramas de latência.
const BOUNDS_US: [u64; 13] = [
    500, 1_000, 2_000, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000, 1_000_000,
    2_500_000, 5_000_000,
];

/// Histograma de latência (baldes fixos, sem alocação no caminho quente).
#[derive(Default)]
pub struct Hist {
    buckets: [AtomicU64; 14], // 13 limites + «mais»
    sum_us: AtomicU64,
    count: AtomicU64,
}

impl Hist {
    pub fn observe(&self, d: Duration) {
        let us = d.as_micros().min(u128::from(u64::MAX / 2)) as u64;
        let i = BOUNDS_US
            .iter()
            .position(|b| us <= *b)
            .unwrap_or(BOUNDS_US.len());
        self.buckets[i].fetch_add(1, Relaxed);
        self.sum_us.fetch_add(us, Relaxed);
        self.count.fetch_add(1, Relaxed);
    }

    fn render(&self, name: &str, help: &str, out: &mut String) {
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} histogram\n"));
        let mut cum = 0;
        for (i, b) in BOUNDS_US.iter().enumerate() {
            cum += self.buckets[i].load(Relaxed);
            out.push_str(&format!(
                "{name}_bucket{{le=\"{}\"}} {cum}\n",
                *b as f64 / 1e6
            ));
        }
        cum += self.buckets[BOUNDS_US.len()].load(Relaxed);
        out.push_str(&format!("{name}_bucket{{le=\"+Inf\"}} {cum}\n"));
        out.push_str(&format!(
            "{name}_sum {}\n{name}_count {}\n",
            self.sum_us.load(Relaxed) as f64 / 1e6,
            self.count.load(Relaxed)
        ));
    }
}

/// Uma etapa agrupada (`batch`): quanto espera quem pede (`total`), quanto demora cada gravação (`flush`) e o tamanho dos lotes.
#[derive(Default)]
pub struct Stage {
    pub total: Hist,
    pub flush: Hist,
    pub rows: AtomicU64,
    pub flushes: AtomicU64,
}

#[derive(Default)]
pub struct Metrics {
    pub enqueued: AtomicU64,
    pub rate_limited: AtomicU64,
    pub shed: AtomicU64,
    pub acked: AtomicU64,
    pub provider_accepted: AtomicU64,
    pub provider_failed: AtomicU64,
    pub ws_opened: AtomicU64,
    pub ws_closed: AtomicU64,
    /// Etapas do caminho quente (ver `batch`): gravação, reserva+envio, ack, aviso a outra instância.
    pub insert: Arc<Stage>,
    pub claim: Arc<Stage>,
    pub ack: Arc<Stage>,
    pub notify: Arc<Stage>,
    /// Do aviso publicado à sua chegada à instância de destino (mesmo relógio: só vale numa máquina, ou com relógios sincronizados).
    pub bus_transit: Hist,
    /// O pedido `POST /v1/messages` inteiro, do ponto de vista do servidor.
    pub send_total: Hist,
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
            "dpush_sends_shed_total",
            "Pedidos de envio recusados por sobrecarga da instância (503).",
            l(&self.shed),
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
        self.send_total.render(
            "dpush_send_seconds",
            "Duração do pedido de envio no servidor.",
            &mut s,
        );
        self.bus_transit.render(
            "dpush_bus_transit_seconds",
            "Do aviso publicado à chegada ao destino (entre instâncias).",
            &mut s,
        );
        for (n, st) in [
            ("insert", &self.insert),
            ("claim", &self.claim),
            ("ack", &self.ack),
            ("notify", &self.notify),
        ] {
            st.total.render(
                &format!("dpush_stage_{n}_seconds"),
                "Espera de quem pede esta etapa (inclui a espera do lote).",
                &mut s,
            );
            st.flush.render(
                &format!("dpush_stage_{n}_flush_seconds"),
                "Duração de cada gravação (lote).",
                &mut s,
            );
            s.push_str(&format!("# TYPE dpush_stage_{n}_rows_total counter\ndpush_stage_{n}_rows_total {}\n# TYPE dpush_stage_{n}_flushes_total counter\ndpush_stage_{n}_flushes_total {}\n", st.rows.load(Relaxed), st.flushes.load(Relaxed)));
        }
        s.push_str(&format!(
            "# HELP dpush_ws_connections Ligações WebSocket vivas nesta instância.\n# TYPE dpush_ws_connections gauge\ndpush_ws_connections {connections_now}\n"
        ));
        s
    }
}
