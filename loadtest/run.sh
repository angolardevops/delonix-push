#!/usr/bin/env bash
# Prova de carga por escalões contra um servidor release com base nova. Cada escalão: CONNS ligações, RATE msg/s, SECS s.
# uso: loadtest/run.sh "1000:200:20" "5000:500:20" ...   (ver docs/carga.md para o que foi medido e em que máquina)
set -uo pipefail
# Uma corrida de cada vez: duas a escrever no mesmo sítio estragam os números (já aconteceu).
exec 9>/tmp/delonix-push-carga.lock
flock -n 9 || { echo "já há uma prova de carga a correr" >&2; exit 3; }
cd "$(dirname "$0")/.."
CONT="${PG_CONTAINER:-push-pg}"; PGU="${PG_USER:-delonix}"; PGP="${PG_PASSWORD:-delonix_dev}"; PGPORT="${PG_PORT:-55533}"
DB="push_carga"; PORT=18481
delonix container exec "$CONT" psql -U "$PGU" -d postgres -c "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
delonix container exec "$CONT" psql -U "$PGU" -d postgres -c "CREATE DATABASE $DB" >/dev/null || exit 2
ulimit -n 1048576 2>/dev/null || true
# INSTANCES=K arranca K servidores sobre a MESMA base (portos PORT..PORT+K-1); o gerador espalha-lhes ligações e pedidos.
K="${INSTANCES:-1}"; PIDS=(); URLS=()
for i in $(seq 0 $((K-1))); do
  P=$((PORT+i))
  DATABASE_URL="postgres://$PGU:$PGP@127.0.0.1:$PGPORT/$DB" PUSH_BIND="127.0.0.1:$P" PUSH_ADMIN_TOKEN=admin-carga PUSH_METRICS_TOKEN=met-carga \
    target/release/delonix-push >/tmp/push-carga-$i.log 2>&1 &
  PIDS+=($!); URLS+=("http://127.0.0.1:$P")
done
SRV="${PIDS[*]}"
trap 'for p in "${PIDS[@]}"; do kill $p 2>/dev/null; done; delonix container exec "$CONT" psql -U "$PGU" -d postgres -c "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1' EXIT
for i in $(seq 0 $((K-1))); do
  for _ in $(seq 50); do [ "$(curl -s -o /dev/null -w '%{http_code}' localhost:$((PORT+i))/healthz)" = 200 ] && break; sleep 0.2; done
done
# DROP_INDEX=<nome>: apaga um índice da base de prova (experiência: medir o que custa à escrita).
if [ -n "${DROP_INDEX:-}" ]; then
  delonix container exec "$CONT" psql -U "$PGU" -d "$DB" -c "DROP INDEX IF EXISTS $DROP_INDEX" >/dev/null 2>&1 && echo "# índice $DROP_INDEX apagado (experiência)"
fi
IFS=, ; URL_LIST="${URLS[*]}"; PID_LIST="${PIDS[*]}"; unset IFS
echo "# $K servidor(es) release (pids ${PIDS[*]}), pool ${PUSH_DB_MAX_CONNECTIONS:-10} cada, base $DB, $(nproc) núcleos, $(date -Is)"
for t in "$@"; do
  IFS=: read -r c r s <<<"$t"
  echo; echo "=== $c ligações · $r msg/s · ${s}s ==="
  snap() { for u in "${URLS[@]}"; do curl -s -H "authorization: Bearer met-carga" "$u/metrics"; done; }
  snap > /tmp/metrics-antes.txt
  loadtest/target/release/delonix-push-loadtest --url "$URL_LIST" --admin-token admin-carga --metrics-token met-carga \
    --conns "$c" --rate "$r" --secs "$s" --server-pid "$PID_LIST"
  snap > /tmp/metrics-depois.txt
  # Médias POR ETAPA neste escalão (soma das instâncias): diferença de `_sum` e `_count` entre antes e depois.
  python3 - <<'PY'
import re
def lê(f):
    d = {}
    for l in open(f):
        m = re.match(r'^(dpush_\w+?)(_sum|_count|_rows_total|_flushes_total) ([\d.e+-]+)$', l.strip())
        if m: d[(m.group(1), m.group(2))] = d.get((m.group(1), m.group(2)), 0) + float(m.group(3))
    return d
a, b = lê('/tmp/metrics-antes.txt'), lê('/tmp/metrics-depois.txt')
print("   por etapa (média neste escalão):")
for nome in sorted({k[0] for k in b}):
    n = b.get((nome, '_count'), 0) - a.get((nome, '_count'), 0)
    if n > 0 and (nome, '_sum') in b:
        print(f"     {nome:44s} n={int(n):7d}  média {1000 * (b[(nome, '_sum')] - a.get((nome, '_sum'), 0)) / n:8.2f} ms")
for st in ('insert', 'claim', 'ack', 'notify'):
    r = b.get((f'dpush_stage_{st}', '_rows_total'), 0) - a.get((f'dpush_stage_{st}', '_rows_total'), 0)
    f = b.get((f'dpush_stage_{st}', '_flushes_total'), 0) - a.get((f'dpush_stage_{st}', '_flushes_total'), 0)
    if f > 0: print(f"     lote médio {st:7s}: {r / f:6.1f} linhas ({int(f)} gravações)")
PY
done
