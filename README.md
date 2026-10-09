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

## API de gestão (`/console/v1`)

Para a consola web (e para CLI/Terraform). `PUSH_SECRET_KEY` (64 hex) cifra as credenciais dos fornecedores; sem ela o servidor recusa guardá-las (503). Os destinos do FCM/APNs são fixos no servidor: nunca vêm das credenciais de um inquilino. Sessão `Bearer dps_…` (`POST /console/v1/auth/login`; registo fechado por omissão,
`PUSH_CONSOLE_OPEN_REGISTRATION=1` abre-o). Papéis por organização: `viewer` < `developer` < `admin` < `owner`.

| Rota | Papel mínimo |
|---|---|
| `POST /console/v1/auth/register｜login｜logout`, `GET /console/v1/me` | — |
| `GET｜POST /console/v1/orgs`, `GET｜POST /console/v1/orgs/{org}/projects` | membro ｜ admin para criar |
| `GET /console/v1/projects/{id}` (aparelhos, ligados agora) | viewer |
| `GET｜POST /console/v1/projects/{id}/keys`, `DELETE …/keys/{key}` | admin (o valor só aparece ao criar) |
| `GET /console/v1/projects/{id}/devices`, `DELETE …/devices/{id}` | viewer ｜ developer |
| `GET /console/v1/projects/{id}/messages?state=` (sem o conteúdo), `POST` (mensagem de teste) | viewer ｜ developer |
| `GET /console/v1/projects/{id}/credentials`, `PUT｜DELETE …/credentials/{fcm｜apns}` | admin (cifradas em repouso; a chave nunca volta) |
| `GET /console/v1/projects/{id}/stats` (24 h: por estado, por hora, latência p50/p95) | viewer |

Um projecto de outra organização responde 404, como se não existisse. As acções de gestão ficam em `audit_log`.

## Consola web

`console/` (React + TypeScript, Vite): entrar/criar conta, organizações, projectos, chaves (o valor só aparece ao criar),
aparelhos (ligados agora), mensagens (estado e erro, sem conteúdo), enviar mensagem de teste, credenciais FCM/APNs, e a
visão geral com as últimas 24 h. Os botões seguem o papel da pessoa na organização.

```bash
(cd console && npm ci && npm run build)
PUSH_CONSOLE_DIR=$PWD/console/dist cargo run      # o servidor serve a consola em /
(cd console && npm run dev)                        # desenvolvimento, com proxy para 127.0.0.1:8480
```

## Limites, métricas e operação

| Variável | Para quê |
|---|---|
| `DATABASE_URL`, `PUSH_BIND` | base e endereço |
| `PUSH_ADMIN_TOKEN` | plano de operador: criar projectos, ajustar limites (`PUT /admin/v1/projects/{id}/limits`) |
| `PUSH_SECRET_KEY` | 64 hex: cifra as credenciais FCM/APNs |
| `PUSH_REDIS_URL` | limites partilhados entre instâncias (sem ela, cada instância conta sozinha) |
| `PUSH_METRICS_TOKEN` | abre `/metrics` (Prometheus) a quem trouxer o token; vazio = fechado |
| `PUSH_CONSOLE_OPEN_REGISTRATION=1` | qualquer pessoa pode criar conta na consola |
| `PUSH_CONSOLE_DIR` | pasta `console/dist`: o servidor serve a consola em `/` |
| `PUSH_DB_MAX_CONNECTIONS` | pool da base (10 por omissão: mais piorou nas medições) |
| `PUSH_ASYNC_COMMIT=1` | commit sem esperar o disco (duplica a escrita; pode perder dezenas de ms num crash) |
| `PUSH_MAX_INFLIGHT_SENDS` | envios em curso por instância (256); acima disso 503 + `Retry-After` |
| `PUSH_DEFAULT_RATE_PER_SEC`, `PUSH_DEFAULT_DAILY_QUOTA` | limites por omissão de cada projecto |

Limites por projecto: mensagens por segundo e por dia (um tópico gasta uma unidade por aparelho). Excedê-los dá `429` com
`Retry-After`. Janela fixa de 1 s: pode passar até ao dobro do limite à volta da fronteira. Se o Redis falhar, deixa passar.

## SDK Android

`sdk/android` (Gradle): `:core` é Kotlin/JVM puro (`PushClient`: WebSocket com ack, deduplicação por `id`, recuo
exponencial com *jitter*, *heartbeat*, pára se o segredo for revogado) e `:android` junta-lhe um serviço em
primeiro plano (`PushService`), `BootReceiver` e a fachada `DelonixPush`. Testes: `./gradlew :core:test` (servidor de
papel) e `sdk/android/e2e.sh` (contra o binário Rust real, com base nova). O serviço Android compila (`assembleDebug`)
mas **ainda não correu num aparelho/emulador**.

## Carga

`loadtest/` + [docs/carga.md](docs/carga.md): o que se mediu (numa instância: 10 000 ligações a 1 000 msg/s com p99 de 2,8 ms; ~2 500 msg/s sustentadas com p99 de 50 ms) e, sobretudo, o que **não** se mediu.

## Licença e contribuições

[Apache-2.0](LICENSE). Contribuições são bem-vindas por pull request; correm `cargo fmt --check`, `cargo clippy --all-targets`
e `cargo test`. Vulnerabilidades: [SECURITY.md](SECURITY.md).
