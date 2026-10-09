//! Gerador de carga do delonix-push. Mede o que um operador quer saber antes de prometer números:
//! quantas ligações aguenta, a que taxa entrega, com que latência (p50/p95/p99) e quanto perde.
//!
//! A latência é medida de ponta a ponta no MESMO relógio (o emissor e os receptores estão neste processo):
//! o emissor põe a hora no `payload.t` (µs) e o receptor subtrai-a à hora de chegada. Não mede a rede real.
//!
//! uso: delonix-push-loadtest --url http://127.0.0.1:8480 --admin-token T --metrics-token M \
//!        --conns 1000 --rate 200 --secs 30 [--server-pid PID]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use hdrhistogram::Histogram;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as T;

fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64
}

fn arg(args: &HashMap<String, String>, k: &str, default: &str) -> String {
    args.get(k).cloned().unwrap_or_else(|| default.to_string())
}

/// (VmRSS em MiB, tempo de CPU em segundos) do processo do servidor, se se souber o PID.
fn proc_stats(pid: u32) -> Option<(f64, f64)> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kb: f64 = status.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1.split_whitespace().collect::<Vec<_>>();
    let ticks: f64 = after.get(11)?.parse::<f64>().ok()? + after.get(12)?.parse::<f64>().ok()?;
    Some((rss_kb / 1024.0, ticks / 100.0))
}

fn load_avg() -> String {
    std::fs::read_to_string("/proc/loadavg").map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" ")).unwrap_or_default()
}

#[tokio::main]
async fn main() {
    let mut args = HashMap::new();
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        if let Some(k) = k.strip_prefix("--") {
            args.insert(k.to_string(), it.next().unwrap_or_default());
        }
    }
    let url = arg(&args, "url", "http://127.0.0.1:8480");
    let admin = arg(&args, "admin-token", "");
    let metrics_token = arg(&args, "metrics-token", "");
    let conns: usize = arg(&args, "conns", "1000").parse().unwrap();
    let rate: u64 = arg(&args, "rate", "200").parse().unwrap();
    let secs: u64 = arg(&args, "secs", "30").parse().unwrap();
    let server_pid: Option<u32> = args.get("server-pid").and_then(|p| p.parse().ok());
    let ws_base = url.replacen("http", "ws", 1);

    let http = reqwest::Client::builder().pool_max_idle_per_host(256).timeout(Duration::from_secs(20)).build().unwrap();

    // 1. projecto e limites altos (o que se mede é o servidor, não o limite)
    let p: Value = http.post(format!("{url}/admin/v1/projects")).header("x-admin-token", &admin)
        .json(&json!({"name": format!("carga-{}", now_us())})).send().await.expect("projecto").json().await.unwrap();
    let key = p["server_key"].as_str().expect("server_key (PUSH_ADMIN_TOKEN certo?)").to_string();
    let pid = p["project_id"].as_str().unwrap().to_string();
    http.put(format!("{url}/admin/v1/projects/{pid}/limits")).header("x-admin-token", &admin)
        .json(&json!({"rate_per_sec": 1_000_000, "daily_quota": 1_000_000_000_i64})).send().await.unwrap();

    // 2. aparelhos (em paralelo)
    let t0 = Instant::now();
    let sem = Arc::new(tokio::sync::Semaphore::new(64));
    let mut jobs = Vec::new();
    for _ in 0..conns {
        let (http, url, key, sem) = (http.clone(), url.clone(), key.clone(), sem.clone());
        jobs.push(tokio::spawn(async move {
            let _g = sem.acquire().await.unwrap();
            let r: Value = http.post(format!("{url}/v1/devices")).bearer_auth(&key).json(&json!({"platform": "android"}))
                .send().await.ok()?.json().await.ok()?;
            Some((r["device_id"].as_str()?.to_string(), r["device_secret"].as_str()?.to_string()))
        }));
    }
    let mut devices: Vec<(String, String)> = Vec::new();
    for j in jobs {
        if let Ok(Some(d)) = j.await {
            devices.push(d);
        }
    }
    println!("aparelhos criados: {}/{} em {:.1}s", devices.len(), conns, t0.elapsed().as_secs_f64());

    // 3. ligações WebSocket: cada uma lê, mede a latência e confirma
    let hist = Arc::new(Mutex::new(Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap()));
    let received = Arc::new(AtomicU64::new(0));
    let connected = Arc::new(AtomicU64::new(0));
    let failed_connect = Arc::new(AtomicU64::new(0));
    let t1 = Instant::now();
    let sem = Arc::new(tokio::sync::Semaphore::new(128));
    for (_, secret) in devices.iter().cloned() {
        let (hist, received, connected, failed, sem, ws_base) = (hist.clone(), received.clone(), connected.clone(), failed_connect.clone(), sem.clone(), ws_base.clone());
        tokio::spawn(async move {
            let g = sem.acquire().await.unwrap();
            let mut req = format!("{ws_base}/v1/connect").into_client_request().unwrap();
            req.headers_mut().insert("authorization", format!("Bearer {secret}").parse().unwrap());
            let Ok((mut ws, _)) = tokio_tungstenite::connect_async(req).await else {
                failed.fetch_add(1, Relaxed);
                return;
            };
            drop(g);
            connected.fetch_add(1, Relaxed);
            while let Some(Ok(m)) = ws.next().await {
                if let T::Text(t) = m {
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    if v["type"] == "message" {
                        if let Some(sent) = v["payload"]["t"].as_u64() {
                            let lat = now_us().saturating_sub(sent).max(1);
                            let _ = hist.lock().unwrap().record(lat);
                        }
                        received.fetch_add(1, Relaxed);
                        let _ = ws.send(T::Text(json!({"type": "ack", "id": v["id"]}).to_string())).await;
                    }
                }
            }
        });
    }
    while connected.load(Relaxed) + failed_connect.load(Relaxed) < devices.len() as u64 && t1.elapsed() < Duration::from_secs(120) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("ligações: {} ligadas, {} falhadas, em {:.1}s", connected.load(Relaxed), failed_connect.load(Relaxed), t1.elapsed().as_secs_f64());
    tokio::time::sleep(Duration::from_secs(2)).await; // deixa a presença assentar

    // 4. emissão a taxa fixa
    let stats0 = server_pid.and_then(proc_stats);
    let sent = Arc::new(AtomicU64::new(0));
    let accepted = Arc::new(AtomicU64::new(0));
    let throttled = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let shed = Arc::new(AtomicU64::new(0));
    let load0 = load_avg();
    let start = Instant::now();
    let total = rate * secs;
    let mut tick = tokio::time::interval(Duration::from_micros(1_000_000 / rate.max(1)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let mut inflight = Vec::new();
    for i in 0..total {
        tick.tick().await;
        let dev = devices[(i as usize) % devices.len()].0.clone();
        let (http, url, key, sent, accepted, throttled, errors, shed) = (http.clone(), url.clone(), key.clone(), sent.clone(), accepted.clone(), throttled.clone(), errors.clone(), shed.clone());
        inflight.push(tokio::spawn(async move {
            sent.fetch_add(1, Relaxed);
            match http.post(format!("{url}/v1/messages")).bearer_auth(&key)
                .json(&json!({"device_id": dev, "payload": {"t": now_us()}, "priority": "high"})).send().await {
                Ok(r) if r.status() == 202 => { accepted.fetch_add(1, Relaxed); }
                Ok(r) if r.status() == 429 => { throttled.fetch_add(1, Relaxed); }
                Ok(r) if r.status() == 503 => { shed.fetch_add(1, Relaxed); }
                _ => { errors.fetch_add(1, Relaxed); }
            }
        }));
        if inflight.len() > 4096 {
            inflight.retain(|h| !h.is_finished());
        }
    }
    for j in inflight {
        let _ = j.await;
    }
    let send_secs = start.elapsed().as_secs_f64();
    tokio::time::sleep(Duration::from_secs(5)).await; // drena
    let stats1 = server_pid.and_then(proc_stats);

    // 5. relatório
    let acc = accepted.load(Relaxed);
    let rec = received.load(Relaxed);
    let h = hist.lock().unwrap();
    let ms = |q: f64| h.value_at_quantile(q) as f64 / 1000.0;
    let server_acks = if metrics_token.is_empty() { None } else {
        http.get(format!("{url}/metrics")).bearer_auth(&metrics_token).send().await.ok()
    };
    let acked = match server_acks { Some(r) => r.text().await.ok().and_then(|t| t.lines().find(|l| l.starts_with("dpush_messages_acked_total")).and_then(|l| l.split(' ').nth(1)?.parse::<u64>().ok())), None => None };
    println!("\n== resultado ==");
    println!("ligações vivas:        {}", connected.load(Relaxed));
    println!("pedidos de envio:      {} (aceites {acc}, limitados {}, recusados por sobrecarga {}, erros {})", sent.load(Relaxed), throttled.load(Relaxed), shed.load(Relaxed), errors.load(Relaxed));
    println!("taxa de envio real:    {:.0} msg/s (alvo {rate})", sent.load(Relaxed) as f64 / send_secs);
    println!("recebidas pelos aparelhos: {rec} ({:.2}% das aceites)", 100.0 * rec as f64 / acc.max(1) as f64);
    if let Some(a) = acked { println!("ack vistos pelo servidor:  {a}"); }
    println!("latência de entrega:   p50 {:.1} ms · p95 {:.1} ms · p99 {:.1} ms · máx {:.1} ms", ms(0.5), ms(0.95), ms(0.99), h.max() as f64 / 1000.0);
    if let (Some((r0, c0)), Some((r1, c1))) = (stats0, stats1) {
        println!("servidor: RSS {r0:.0} → {r1:.0} MiB · CPU {:.1}s em {send_secs:.0}s ({:.0}% de um núcleo)", c1 - c0, 100.0 * (c1 - c0) / send_secs);
    }
    println!("anfitrião: nproc {} · load {} → {}", std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0), load0, load_avg());
}
