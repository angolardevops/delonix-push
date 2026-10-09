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
DATABASE_URL="postgres://$PGU:$PGP@127.0.0.1:$PGPORT/$DB" PUSH_BIND="127.0.0.1:$PORT" PUSH_ADMIN_TOKEN=admin-carga PUSH_METRICS_TOKEN=met-carga \
  target/release/delonix-push >/tmp/push-carga.log 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null; delonix container exec "$CONT" psql -U "$PGU" -d postgres -c "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1' EXIT
for _ in $(seq 50); do [ "$(curl -s -o /dev/null -w '%{http_code}' localhost:$PORT/healthz)" = 200 ] && break; sleep 0.2; done
echo "# servidor release (pid $SRV), base $DB, $(nproc) núcleos, $(date -Is)"
for t in "$@"; do
  IFS=: read -r c r s <<<"$t"
  echo; echo "=== $c ligações · $r msg/s · ${s}s ==="
  loadtest/target/release/delonix-push-loadtest --url "http://127.0.0.1:$PORT" --admin-token admin-carga --metrics-token met-carga \
    --conns "$c" --rate "$r" --secs "$s" --server-pid "$SRV"
done
