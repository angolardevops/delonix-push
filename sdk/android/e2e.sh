#!/usr/bin/env bash
# O SDK contra o servidor real: base nova, binário do cargo, testes do Gradle com as variáveis do e2e.
# Precisa de um Postgres no delonix (CONTAINER, por omissão push-pg) e de JAVA_HOME (JDK 17+).
set -euo pipefail
cd "$(dirname "$0")"
CONT="${PG_CONTAINER:-push-pg}"; PGU="${PG_USER:-delonix}"; PGP="${PG_PASSWORD:-delonix_dev}"; PGPORT="${PG_PORT:-55533}"
DB="push_e2e_$$"
delonix container exec "$CONT" psql -U "$PGU" -d postgres -c "CREATE DATABASE $DB" >/dev/null
trap 'delonix container exec "$CONT" psql -U "$PGU" -d postgres -c "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1 || true' EXIT
(cd ../.. && cargo build -q)
DELONIX_PUSH_BIN="$(cd ../.. && pwd)/target/debug/delonix-push" \
DELONIX_PUSH_DB="postgres://$PGU:$PGP@127.0.0.1:$PGPORT/$DB" \
  ./gradlew --no-daemon -q :core:test --rerun-tasks
echo "✓ SDK Android contra o servidor real"
