# ADR-0002 — delonix-push como SaaS: consola de gestão e escala

**Estado:** Proposto · **Data:** 2026-10-09 · Sucede à secção «Limites conhecidos da v1» do ADR-0001.

## Objectivo

Um serviço parecido com o Firebase Cloud Messaging, multi-inquilino, com consola web de gestão, que aguente **dezenas de
milhares de ligações simultâneas por nó** e **milhares de mensagens por segundo** no total, escalando por réplicas.
Os números abaixo são **metas**; só passam a ser afirmações depois de medidos (ver «Prova de carga»).

## Dois planos

**Plano de dados** (já existe, a endurecer): gateway WebSocket sem estado local relevante + API de envio. Escala por
réplicas atrás de um balanceador de TCP/WebSocket. Cada instância sabe só das ligações que tem; a presença global vive
numa base partilhada (hoje `device_connections` no Postgres).

**Plano de controlo** (novo): contas, organizações, projectos, chaves, credenciais de fornecedor, quotas, métricas e a
**consola web**. Fala com a mesma base mas nunca fica no caminho quente de uma mensagem.

## Modelo de inquilinos

`account` (pessoa, entra na consola) → `organization` (equipa, plano e facturação) → `project` (a «app») → chaves,
aparelhos, tópicos, mensagens. Todas as consultas levam `project_id` (já assim); a consola acrescenta `organization_id`
e papéis (`owner`, `admin`, `developer`, `viewer`). Um projecto nunca vê outro (teste com duas organizações, como no Meet).

Chaves: de **servidor** (`dpk_…`, envia e gere), de **cliente** (`dpc_…`, só regista o próprio aparelho: o que falta para
registo sem passar pelo servidor da app) e de **leitura** (métricas). Todas com escopo, rotação e revogação; só o hash fica na base.

## Caminho quente e como escala

| Ponto | Hoje | Alvo |
|---|---|---|
| Fila de mensagens | Postgres (`messages`) | Postgres para metadados e estado final; **fila quente em Redis Streams** (ou NATS JetStream): consumo por grupo, *ack*, reentrega. A tabela `messages` passa a particionada por dia e arrumada por TTL. |
| Presença das ligações | tabela com *heartbeat* | Redis (`SET device:{id} node EX 120`), com a tabela só como recuo |
| Entre instâncias | `LISTEN/NOTIFY` | Redis pub/sub (ou NATS): o `NOTIFY` do Postgres não escala para muitos nós/mensagens |
| Limites por projecto | nenhum | *token bucket* em Redis: envios/s, mensagens/dia, aparelhos, tamanho; resposta 429 com `Retry-After` |
| Ligações por nó | uma tarefa Tokio por ligação, fila de 256 | ajustar `ulimit`, buffers e *backpressure*; alvo de 50 000 ligações por nó com 2 vCPU/4 GB, a **medir** |
| Difusão de tópico | síncrona, ≤ 1000 | *fan-out* assíncrono em lotes pelos workers |
| Fornecedores FCM/APNs | pedido por mensagem | HTTP/2 multiplexado e *pool*, lotes onde o fornecedor deixa, recuo e *circuit breaker* por projecto |

Estado nas instâncias: nenhum que não se reconstrua. Perder um nó = os aparelhos reconectam (recuo com *jitter* já no SDK),
as mensagens em voo voltam à fila pelo lease expirado.

## Consola web (`console/`)

SPA em React/TypeScript servida pelo próprio binário ou por nginx. Ecrãs: entrar (e-mail + senha com TOTP; OIDC depois),
organizações e membros, projectos, chaves (criar/rodar/revogar, mostradas uma só vez), credenciais FCM e APNs (carregadas,
**cifradas em repouso**, nunca devolvidas), aparelhos (estado, última ligação, revogar), tópicos, **enviar mensagem de
teste**, registo de entregas com filtro por estado e pesquisa por `id`, gráficos (enviadas, entregues, falhadas, latência
p50/p95, ligações vivas), uso contra a quota e o plano. A API de gestão é a mesma que um cliente de CLI/Terraform usaria
(`/console/v1/…`), com o seu próprio OpenAPI.

## Segurança e operação

- Credenciais de fornecedor (conta de serviço do Firebase, `.p8` da Apple): cifradas com chave de instalação, nunca em logs.
- Auditoria das acções de gestão (quem criou/rodou/revogou o quê).
- Métricas Prometheus (`/metrics`: ligações, mensagens por estado, latência de entrega, filas, erros por fornecedor),
  *tracing* OpenTelemetry, SLO de entrega e de disponibilidade.
- Deploy: imagem OCI, *Helm chart*/manifestos com HPA por ligações, PodDisruptionBudget e *drain* (fechar ligações com
  `1012 Service Restart` para a reconexão espalhada).

## Prova de carga (obrigatória antes de afirmar números)

Gerador em Rust no repo (`loadtest/`): N ligações WebSocket simuladas com ack, M mensagens/s, mede latência de entrega
(p50/p95/p99), perda e uso de memória/CPU por nó. Corre local (esta máquina é partilhada e ruidosa: regista-se a carga do
anfitrião) e em CI com valores baixos como regressão. Só se publicam números medidos, com o ambiente descrito.

## Fases

1. **Endurecer o plano de dados:** métricas, limites por projecto (Redis), `dpc_`, presença e pub/sub em Redis, partição das mensagens, *drain*.
2. **API de gestão** (`/console/v1`): contas, organizações, projectos, chaves, credenciais, aparelhos, mensagens, estatísticas, auditoria.
3. **Consola web.**
4. **Prova de carga** e afinação; só então os números entram na documentação.
5. **Facturação e planos** (fora deste ADR): contadores de uso exportáveis.

## O que isto não resolve

FCM/APNs continuam a depender das contas da Google e da Apple. O iPhone só recebe push com a app morta por APNs. A escala
da ligação própria no Android esbarra na política de bateria dos fabricantes, não no servidor.

## Estado medido (2026-10-09)

Ver [docs/carga.md](../carga.md). Em resumo, numa instância, em desenvolvimento: 10 000 ligações a 1 000 msg/s com p99 de 2,8 ms e
123 MiB; teto sustentado de ~2 500 msg/s (p99 50 ms) e pico de ~3 300 msg/s numa instância, depois do commit em grupo (antes eram
~1 100), com recusa precoce (503) acima disso. As metas de «dezenas de milhares de ligações por nó» e «milhares de mensagens por
segundo» confirmam-se **numa instância e neste ambiente**; a escala horizontal (várias instâncias) continua por medir, e a fila
quente fora do Postgres deixa de ser urgente (fica para quando o Postgres voltar a ser o teto).

### Actualização (várias instâncias)

O barramento entre instâncias passou para o Redis (pub/sub, `src/bus.rs`) e o aviso leva o aparelho; a escala horizontal foi medida e
é **inconclusiva** (piso de latência de 200–500 ms com mais de uma instância, causa por isolar). Ver docs/carga.md.
