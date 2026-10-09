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

## SDK Android

`sdk/android` (Gradle): `:core` é Kotlin/JVM puro (`PushClient`: WebSocket com ack, deduplicação por `id`, recuo
exponencial com *jitter*, *heartbeat*, pára se o segredo for revogado) e `:android` junta-lhe um serviço em
primeiro plano (`PushService`), `BootReceiver` e a fachada `DelonixPush`. Testes: `./gradlew :core:test` (servidor de
papel) e `sdk/android/e2e.sh` (contra o binário Rust real, com base nova). O serviço Android compila (`assembleDebug`)
mas **ainda não correu num aparelho/emulador**.

## Licença e contribuições

[Apache-2.0](LICENSE). Contribuições são bem-vindas por pull request; correm `cargo fmt --check`, `cargo clippy --all-targets`
e `cargo test`. Vulnerabilidades: [SECURITY.md](SECURITY.md).
