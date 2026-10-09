# Prova de carga — o que se mediu e o que não

Gerador: `loadtest/` (N ligações WebSocket reais com ack, M mensagens/s, latência de entrega de ponta a ponta no mesmo
relógio, perda, RSS e CPU do servidor). Corre com `loadtest/run.sh "<ligações>:<msg/s>:<segundos>" …` (servidor release,
base nova, uma corrida de cada vez).

## Ambiente (limita o alcance dos números)

Um portátil/estação de 32 núcleos e 30 GB **partilhada com outras sessões** (carga de fundo entre 3 e 12 durante as
medições). Servidor e gerador **na mesma máquina** (a rede é o *loopback*, não é rede real). O Postgres corre num contentor
rootless atrás de rede em modo utilizador (`tap0`), com o porto publicado em `127.0.0.1`. **Uma só instância do servidor.**
Estes números são o piso de um ambiente de desenvolvimento, não a capacidade de um deploy de produção.

## O que se mediu

**Depois do commit em grupo** (`src/batch.rs`: as escritas concorrentes juntam-se numa só instrução; o 202 só sai depois de o
lote estar gravado). Servidor release, commit síncrono, pool de 10 ligações, 2 000 ligações abertas:

| Taxa oferecida | Aceite | Recusado (503) | Entregue (do aceite) | Latência p50 · p95 · p99 | CPU do servidor |
|---|---|---|---|---|---|
| 1 000 msg/s | 1 000/s | 0 | 100 % | 4,8 · 6,1 · 6,7 ms | 26 % de um núcleo |
| 2 500 msg/s | 2 500/s | 0 | 100 % | 5,0 · 34,9 · 49,7 ms | 49 % |
| 4 000 msg/s | ~3 270/s | 22 % | 100 % | 46,7 · 86,0 · 120 ms | 72 % |
| 6 000 msg/s | ~2 580/s | 57 % | 100 % | 88,5 · 169,5 · 187,6 ms | 126 % |

**Capacidade sustentada por instância neste ambiente: ~2 500 mensagens/s sem recusar nada (p99 50 ms); pico aceite ~3 300/s.**
Acima disso o servidor recusa cedo (503 + `Retry-After`) e protege quem já foi aceite; a 6 000 msg/s oferecidas o aceite até desce
(~2 580/s), porque a sobrecarga também custa trabalho — o limite deve estar à frente (balanceador), não só aqui.
Antes do commit em grupo o teto era ~1 000–1 200 msg/s.

Outros números (medidos antes do commit em grupo, a memória não mudou):

| Cenário | Resultado |
|---|---|
| 10 000 ligações · 1 000 msg/s · 15 s | 100 % entregues · p50 1,7 ms · p99 2,8 ms · RSS 123 MiB |
| 25 000 ligações abertas | todas ligadas em ~12 s; RSS ≈ 1 GiB |

## Várias instâncias (escala horizontal) — resultado INCONCLUSIVO

Ensaio: 1, 2 e 4 instâncias do servidor sobre a MESMA base, 2 000 ligações espalhadas por elas, cada pedido de envio a cair numa
instância à sorte (como atrás de um balanceador: com K instâncias, K-1 em cada K envios tocam uma instância que não tem a ligação
do aparelho). O que se vê, em corridas ruidosas (a máquina é partilhada: a mesma configuração de 1 instância deu p50 de 5,6 ms
numa corrida e de 71 ms noutra):

- **Capacidade:** com 4 instâncias aceitou-se ~3 600 msg/s a 4 000 oferecidas (90 %), contra ~3 000/s com 1 instância. Parece subir
  um pouco, longe de linear, e o ruído da máquina é do mesmo tamanho que a diferença.
- **Latência:** com mais de uma instância o p50 fica em **200–500 ms** (com 1 instância, 5–25 ms). Este piso **não foi explicado**.
- **Hipóteses testadas e descartadas** (cada uma foi uma alteração + nova medição): pool de ligações por instância (3, 5 ou 10),
  `NOTIFY` do Postgres contra o pub/sub do Redis (o Redis não mudou a latência), o worker de reenvio (desligado), a consulta extra
  que o destino fazia por aviso (o aviso passou a levar o aparelho), e o tamanho dos lotes (espera de 1, 5 e 10 ms).
- **Uma corrida de 1 instância com 5 ms de espera colapsou** (4 436 aceites, 936 erros). Não se repetiu nem se explicou: trata-se como
  ruído até haver repetição.

(A causa foi depois isolada com métricas por etapa: ver a secção seguinte.) O que ficou (útil por si): o barramento entre instâncias em Redis (`PUSH_REDIS_URL`; sem ele,
`LISTEN/NOTIFY`), o aviso que leva `<mensagem>@<aparelho>` em lote, a cache de presença (2 s) e de aparelhos validados (3 s), e
`PUSH_BATCH_LINGER_MS`. **Não se deve anunciar escala horizontal enquanto isto não estiver resolvido.**

## Onde se gastam os 200–500 ms com várias instâncias (medido por etapa)

O servidor expõe, em `/metrics`, a latência de cada etapa do caminho quente e o tamanho médio dos lotes
(`dpush_send_seconds`, `dpush_stage_{insert,claim,ack,notify}_seconds` e `_flush_seconds`, `dpush_bus_transit_seconds`,
`dpush_stage_*_rows_total`/`_flushes_total`). `loadtest/run.sh` imprime as médias de cada escalão. Resultado (2 000 ligações):

| | 1 instância, 2 500 msg/s | 4 instâncias, 2 500 msg/s |
|---|---|---|
| trânsito do aviso entre instâncias (Redis) | — | **0,2 ms** |
| gravação de um lote (insert / reserva / ack) | 3,6 / 3,7 / 3,1 ms | **87 / 90 / 73 ms** |
| lote médio (linhas) | 6,8 / 9,3 / 10,2 | 9,2 / 13,3 / 19,6 |
| pedido de envio completo | 18 ms | 396 ms |

**Conclusões:**

1. **O barramento entre instâncias não é o problema** (0,2 ms). O `NOTIFY` do Postgres também não explicava: o Redis não mudou nada.
2. **O custo está na base:** o mesmo trabalho total passa a demorar ~25× mais por gravação quando há 4 instâncias a escrever ao mesmo tempo
   (16 gravações concorrentes de lote em vez de 4). Os lotes não encolhem, por isso não é falta de agrupamento.
3. **Parte da causa é um ponto quente de índice:** todas as mensagens novas têm `next_attempt_at ≈ agora`, logo insert, reserva e ack batem
   na mesma folha do índice `messages_due`. Experiência com esse índice apagado (só na base de prova): as gravações descem de 87/90/73 ms
   para 32/31/25 ms (2 500 msg/s) e de 65/75/67 para 40/43/37 ms (4 000 msg/s). **Ajuda 2–3×, mas não explica tudo** (continuam 8× acima
   do caso de 1 instância).
4. A 1 instância a 4 000 msg/s (acima da capacidade) as mesmas gravações vão a 140–210 ms: a base satura, e o servidor recusa cedo.

**Leitura:** com este Postgres (um contentor rootless, na mesma máquina partilhada) a base é o teto, e ter mais instâncias da aplicação
só lhe põe mais escritas concorrentes. Aumentar o número de instâncias **não aumenta a capacidade enquanto a base for a mesma**.

**O que se pode fazer a seguir** (por ordem de custo/benefício, nenhum feito):
- Trocar o índice `messages_due` por um que não seja um ponto quente (por exemplo só `WHERE state = 'sent'` para o reenvio por falta de ack,
  e um índice próprio e pequeno para as `queued` que precisam de reencaminhamento). **Mexe na correção do reenvio**: precisa de testes
  dos três caminhos (aparelho offline, fornecedor com falha transitória, presença desactualizada).
- Reduzir as escritas por mensagem de 3 para 1–2: o estado `sent` e o `delivered` podiam fundir-se (gravar só a confirmação) no caminho de
  ligação própria, guardando o «enviado» em memória.
- Medir com um Postgres nativo, ajustado e com rede real: este ambiente não diz a capacidade de produção.
- A fila quente fora do Postgres (Redis Streams ou NATS), como previsto no ADR-0002, é a saída definitiva: tira as escritas do caminho quente.

## O que a medição apanhou e foi corrigido

1. **Memória por ligação:** ~190 KB (buffers de 128 KiB de leitura e de escrita do WebSocket por omissão) → ~38 KB com buffers de
   4 KiB. 25 000 ligações passaram de 4,8 GB para ~1 GiB.
2. **Sem protecção de sobrecarga:** a 2× a capacidade a latência passava de 10 s, a memória multiplicava-se por dez e o gerador
   tinha timeouts. Agora há um limite de envios em curso (`PUSH_MAX_INFLIGHT_SENDS`, 256) com 503 + `Retry-After`.
3. **Consultas por mensagem:** de ~9 para 3 (chave e limites em cache de 3 s e 5 s; INSERT sem transacção no caminho comum;
   reserva e marcação de «enviada» numa só consulta), e **commit em grupo**: as 3 escritas de cada mensagem juntam-se com as das
   outras mensagens concorrentes (até 128 por instrução, 1 ms de espera): o teto passou de ~1 100 para ~2 500–3 300 msg/s.
4. **`from_env` não lia `PUSH_SECRET_KEY` nem `PUSH_METRICS_TOKEN`** (as credenciais dariam sempre 503 e o `/metrics` sempre 403;
   os testes não o viam porque configuravam o `Config` à mão). Corrigido, com teste.

## Resultados contraintuitivos (e porquê)

- **Mais ligações à base pioram:** pool de 10 → p50 2 ms; 20 → 126 ms; 50 → 8 s (a 5 000 ligações · 1 000 msg/s). O próprio Postgres
  mostra o mesmo: `pgbench` (INSERT) dá 11,8 mil tps com 10 clientes e 9,8 mil com 50. O pool por omissão é por isso **10**, e é por
  isso que se agrupam as escritas em vez de lhes dar mais ligações.
- **`synchronous_commit=off`** duplica o débito de escrita do Postgres (22,5 mil vs 11,8 mil inserts/s) e baixa a latência
  (p50 0,7 ms), mas (medido ANTES do commit em grupo) não mudava o teto: o gargalo não era o `fsync`, eram as escritas individuais. Com o
  commit em grupo não se voltou a medir. Fica opcional (`PUSH_ASYNC_COMMIT=1`),
  com o custo de poder perder as últimas dezenas de ms de mensagens aceites num crash do Postgres.

## O que NÃO está medido

- **Várias instâncias:** medidas mas **inconclusivas** (ver acima).
- **Rede real** (latência, perdas, reconexões em massa), **TLS**, e dispositivos reais (bateria, Doze).
- **Postgres em produção** (nativo, com rede real, ajustado): o teto de ~2 500–3 300 msg/s é deste ambiente.
- **FCM/APNs** reais e o seu débito.
- Mais de ~25 000 ligações numa instância (esbarra nos ~28 000 portos efémeros do *loopback*; seria preciso gerar a carga de vários endereços).

## Para passar o teto

Várias instâncias atrás de um balanceador de ligações (a escala horizontal está por medir) e, se o Postgres voltar a ser o teto,
a fila quente fora dele (Redis Streams ou NATS JetStream), como desenhado no ADR-0002. O commit em grupo já tirou ~3× do teto sem
abdicar da durabilidade, por isso a fila externa deixa de ser a primeira alavanca.
