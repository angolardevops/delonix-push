# delonix-push

Push da N'GolaCloud: entrega de mensagens a aplicações móveis, com ligação própria (WebSocket, com *ack*) e
adaptadores FCM/APNs. Desenho e limites em [docs/adr/0001-delonix-push.md](docs/adr/0001-delonix-push.md).

```bash
export DATABASE_URL=postgres://… PUSH_ADMIN_TOKEN=… PUSH_BIND=0.0.0.0:8480
cargo run
```

| Quem | Rota | Autenticação |
|---|---|---|
| operador | `POST /admin/v1/projects {name}` → `project_id`, `server_key` | `X-Admin-Token` |
| servidor da app | `POST /v1/devices {platform}` → `device_id`, `device_secret` | `Bearer dpk_…` |
| servidor da app | `POST /v1/messages {device_id｜topic, payload, priority, ttl_secs, collapse_key, idempotency_key}` | `Bearer dpk_…` |
| servidor da app | `GET /v1/messages/{id}`, `DELETE /v1/devices/{id}`, `PUT｜DELETE /v1/devices/{id}/topics/{t}` | `Bearer dpk_…` |
| aparelho | `GET /v1/connect` (WebSocket), `PUT /v1/device/provider {provider, token}` | `Bearer dpd_…` |

Estados de uma mensagem: `queued → sent → delivered` (ligação própria), `queued → accepted` (FCM/APNs),
`expired`, `failed`. Protocolo do WebSocket em `src/gateway.rs`.

Testes: `DATABASE_URL=… cargo test` (Postgres; `sqlx::test`).
