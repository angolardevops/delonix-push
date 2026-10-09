# Prova de carga — o que se mediu e o que não

Gerador: `loadtest/` (N ligações WebSocket reais com ack, M mensagens/s, latência de entrega de ponta a ponta no mesmo
relógio, perda, RSS e CPU do servidor). Corre com `loadtest/run.sh "<ligações>:<msg/s>:<segundos>" …` (servidor release,
base nova, uma corrida de cada vez).

## Ambiente (limita o alcance dos números)

Um portátil/estação de 32 núcleos e 30 GB **partilhada com outras sessões** (carga de fundo entre 3 e 12 durante as
medições). Servidor e gerador **na mesma máquina** (a rede é o *loopback*, não é rede real). O Postgres corre num contentor
rootless atrás de rede em modo utilizador (`tap0`), com o porto publicado em `127.0.0.1`. **Uma só instância do servidor.**
Estes números são o piso de um ambiente de desenvolvimento, não a capacidade de um deploy de produção.

## O que se mediu (servidor release, commit síncrono, pool de 10 ligações à base)

| Cenário | Resultado |
|---|---|
| 10 000 ligações · 1 000 msg/s · 15 s | 100 % entregues · latência p50 1,7 ms · p95 2,2 ms · p99 2,8 ms · 33 % de um núcleo · RSS 123 MiB |
| 10 000 ligações · 1 000 msg/s, `PUSH_ASYNC_COMMIT=1` | p50 0,7 ms · p95 1,2 ms · p99 1,5 ms |
| 25 000 ligações abertas (sem emissão intensa) | todas ligadas em ~12 s; RSS ≈ 1 GiB |
| 2 000 ligações · 2 500 msg/s (2,5× a capacidade) | aceita ~1 230 msg/s, recusa o resto com 503 · **latência do aceite p50 200 ms, p99 280 ms** · RSS 83 MiB · 100 % do aceite entregue |
| 2 000 ligações · 4 000 msg/s (4×) | aceita ~1 015 msg/s, recusa o resto com 503 · p50 278 ms · p99 390 ms · RSS 105 MiB |

**Capacidade sustentada por instância neste ambiente: ~1 000–1 200 mensagens/s** (cada mensagem custa 3 consultas à base
mais o ack). Acima disso o servidor recusa cedo (503 + `Retry-After`) e protege quem já foi aceite.

## O que a medição apanhou e foi corrigido

1. **Memória por ligação:** ~190 KB (buffers de 128 KiB de leitura e de escrita do WebSocket por omissão) → ~38 KB com buffers de
   4 KiB. 25 000 ligações passaram de 4,8 GB para ~1 GiB.
2. **Sem protecção de sobrecarga:** a 2× a capacidade a latência passava de 10 s, a memória multiplicava-se por dez e o gerador
   tinha timeouts. Agora há um limite de envios em curso (`PUSH_MAX_INFLIGHT_SENDS`, 256) com 503 + `Retry-After`.
3. **Consultas por mensagem:** de ~9 para 3 (chave e limites em cache de 3 s e 5 s; INSERT sem transacção no caminho comum;
   reserva e marcação de «enviada» numa só consulta).
4. **`from_env` não lia `PUSH_SECRET_KEY` nem `PUSH_METRICS_TOKEN`** (as credenciais dariam sempre 503 e o `/metrics` sempre 403;
   os testes não o viam porque configuravam o `Config` à mão). Corrigido, com teste.

## Resultados contraintuitivos (e porquê)

- **Mais ligações à base pioram:** pool de 10 → p50 2 ms; 20 → 126 ms; 50 → 8 s (a 5 000 ligações · 1 000 msg/s). O próprio Postgres
  mostra o mesmo: `pgbench` (INSERT) dá 11,8 mil tps com 10 clientes e 9,8 mil com 50. O pool por omissão é por isso **10**.
- **`synchronous_commit=off`** duplica o débito de escrita do Postgres (22,5 mil vs 11,8 mil inserts/s) e baixa a latência
  (p50 0,7 ms), mas não muda o teto de ~1 000–1 200 msg/s: o gargalo não é o `fsync`. Fica opcional (`PUSH_ASYNC_COMMIT=1`),
  com o custo de poder perder as últimas dezenas de ms de mensagens aceites num crash do Postgres.

## O que NÃO está medido

- **Várias instâncias** sob carga (o código tem presença e `NOTIFY`, testado funcionalmente, mas a escala horizontal não foi medida).
- **Rede real** (latência, perdas, reconexões em massa), **TLS**, e dispositivos reais (bateria, Doze).
- **Postgres em produção** (nativo, com rede real, ajustado): o teto de ~1 000–1 200 msg/s é deste ambiente.
- **FCM/APNs** reais e o seu débito.
- Mais de ~25 000 ligações numa instância (esbarra nos ~28 000 portos efémeros do *loopback*; seria preciso gerar a carga de vários endereços).

## Para passar o teto

O caminho já desenhado no ADR-0002: a fila quente fora do Postgres (Redis Streams ou NATS JetStream), com o Postgres só para o
estado final, escrito em lotes; mais instâncias atrás de um balanceador de ligações.
