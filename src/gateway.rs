//! A ligação própria: um WebSocket por aparelho, com entrega, *ack* e *ping*.
//!
//! Protocolo (texto JSON). Servidor → app: `{"type":"message","id","payload","priority",...}`,
//! `{"type":"pong"}`. App → servidor: `{"type":"ack","id"}`, `{"type":"ping"}`.
//! Autenticação: `Authorization: Bearer dpd_…` no pedido de upgrade. A app tem de deduplicar por `id`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::ws::{Message as WsMsg, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::store::{self, Device};
use crate::{dispatch, AppState};

#[derive(Default)]
pub struct Registry {
    conns: Mutex<HashMap<Uuid, (u64, mpsc::Sender<String>)>>,
    next: Mutex<u64>,
}

impl Registry {
    fn attach(&self, dev: Uuid, tx: mpsc::Sender<String>) -> u64 {
        let mut n = self.next.lock().unwrap();
        *n += 1;
        // Uma ligação nova substitui a antiga (o `Sender` antigo cai e a tarefa antiga termina).
        self.conns.lock().unwrap().insert(dev, (*n, tx));
        *n
    }

    fn detach(&self, dev: Uuid, id: u64) {
        let mut c = self.conns.lock().unwrap();
        if c.get(&dev).map(|(i, _)| *i) == Some(id) {
            c.remove(&dev);
        }
    }

    /// Entrega uma linha à ligação viva do aparelho. `false` = sem ligação (ou cheia).
    pub fn send(&self, dev: Uuid, line: String) -> bool {
        match self.conns.lock().unwrap().get(&dev) {
            Some((_, tx)) => tx.try_send(line).is_ok(),
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.conns.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_connected(&self, dev: Uuid) -> bool {
        self.conns.lock().unwrap().contains_key(&dev)
    }
}

const IDLE: Duration = Duration::from_secs(90);

pub async fn serve(st: AppState, dev: Device, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<String>(256);
    let conn = st.gateway.attach(dev.id, tx);
    crate::metrics::Metrics::inc(&st.metrics.ws_opened);
    store::touch_device(&st.db, dev.id).await;
    let _ = store::presence_set(&st.db, dev.id, st.node_id).await;
    st.presence_cache.put(dev.id, Some(st.node_id));

    // Escritor: tudo o que sai passa por aqui.
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if sink.send(WsMsg::Text(line.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Enquanto a ligação viver, a presença renova-se (um cliente que nunca manda `ping` também conta).
    let beat = {
        let (db, dev_id, node) = (st.db.clone(), dev.id, st.node_id);
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(30));
            loop {
                iv.tick().await;
                store::presence_touch(&db, dev_id, node).await;
            }
        })
    };

    // Ao ligar, entrega o que ficou à espera (inclui o que foi enviado e nunca confirmado).
    if let Ok(open) = store::open_for_device(&st.db, dev.id).await {
        let lease = st.cfg.ack_timeout.as_secs_f64();
        for m in open {
            if st.gateway.send(dev.id, dispatch::wire(&m)) {
                let _ = store::mark_sent(&st.db, m.id, lease).await;
            }
        }
    }

    loop {
        let next = tokio::time::timeout(IDLE, stream.next()).await;
        let Ok(Some(Ok(msg))) = next else { break };
        let WsMsg::Text(t) = msg else {
            if matches!(msg, WsMsg::Close(_)) {
                break;
            } else {
                continue;
            }
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
            continue;
        };
        match v["type"].as_str() {
            Some("ack") => {
                if let Some(id) = v["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) {
                    if st.batch.ack.submit((id, dev.id)).await == Some(true) {
                        crate::metrics::Metrics::inc(&st.metrics.acked);
                    }
                }
            }
            Some("ping") => {
                st.gateway.send(dev.id, r#"{"type":"pong"}"#.into());
                store::touch_device(&st.db, dev.id).await;
                store::presence_touch(&st.db, dev.id, st.node_id).await;
            }
            _ => {}
        }
    }
    st.gateway.detach(dev.id, conn);
    crate::metrics::Metrics::inc(&st.metrics.ws_closed);
    beat.abort();
    // Só se não houve reconexão nesta mesma instância entretanto (a ligação nova já registou a sua presença).
    if !st.gateway.is_connected(dev.id) {
        store::presence_clear(&st.db, dev.id, st.node_id).await;
    }
    writer.abort();
}
