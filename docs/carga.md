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

- **Várias instâncias** sob carga (o código tem presença e `NOTIFY`, testado funcionalmente, mas a escala horizontal não foi medida).
- **Rede real** (latência, perdas, reconexões em massa), **TLS**, e dispositivos reais (bateria, Doze).
- **Postgres em produção** (nativo, com rede real, ajustado): o teto de ~2 500–3 300 msg/s é deste ambiente.
- **FCM/APNs** reais e o seu débito.
- Mais de ~25 000 ligações numa instância (esbarra nos ~28 000 portos efémeros do *loopback*; seria preciso gerar a carga de vários endereços).

## Para passar o teto

Várias instâncias atrás de um balanceador de ligações (a escala horizontal está por medir) e, se o Postgres voltar a ser o teto,
a fila quente fora dele (Redis Streams ou NATS JetStream), como desenhado no ADR-0002. O commit em grupo já tirou ~3× do teto sem
abdicar da durabilidade, por isso a fila externa deixa de ser a primeira alavanca.
