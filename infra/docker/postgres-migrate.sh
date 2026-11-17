#!/usr/bin/env bash
# Initialisation script for the postgres/citus image used by docker-compose.prod.yml.
#
# The upstream entrypoint runs *every* file in /docker-entrypoint-initdb.d in
# LC_ALL=C order. Mounting libs/migrations/postgresql there verbatim is wrong for
# two reasons:
#
#   1. it also picks up rollback scripts. "014_payments_core.down.sql" sorts
#      before "014_payments_core.sql" and executes DROP TABLE payments AS WELL AS
#      withdrawals before those tables are created.
#   2. psql without ON_ERROR_STOP reports success on partial failures, so a
#      broken migration would be silently half-applied on a fresh volume.
#
# This wrapper filters *.down.sql out, applies the rest with ON_ERROR_STOP=1 and
# records each applied file in schema_migrations - the same bookkeeping
# tools/migrate.sh performs.
#
# NOTE: the postgres entrypoint *sources* non-executable *.sh files and *executes*
# executable ones. Everything here therefore lives in main() and runs in a
# subshell: a stray `exit` or `set -e` must not abort the entrypoint itself.
#
# NOTE: like every initdb script this runs once, when the data directory is
# empty. An existing volume is not re-migrated; use tools/migrate.sh for that.

MIGRATIONS_DIR="${MIGRATIONS_DIR:-/opt/opus-migrations}"

main() (
  set -euo pipefail

  if [ ! -d "${MIGRATIONS_DIR}" ]; then
    echo "[migrations] ${MIGRATIONS_DIR} not found - skipping"
    return 0
  fi

  psql_migrate() {
    local file="$1"
    local name
    name="$(basename "$file")"

    if [ "$(psql -v ON_ERROR_STOP=1 -tAc "SELECT COUNT(*) FROM schema_migrations WHERE filename='${name}'")" != "0" ]; then
      echo "[migrations] [skip]    ${name}"
      return 0
    fi

    echo "[migrations] [apply]   ${name}"
    psql -v ON_ERROR_STOP=1 -f "$file"
    psql -v ON_ERROR_STOP=1 -c \
      "INSERT INTO schema_migrations (filename) VALUES ('${name}') ON CONFLICT DO NOTHING;"
  }

  psql -v ON_ERROR_STOP=1 <<'SQL'
CREATE TABLE IF NOT EXISTS schema_migrations (
  filename   TEXT PRIMARY KEY,
  applied_at TIMESTAMPTZ DEFAULT NOW()
);
SQL

  # Sort by filename, never apply *.down.sql.
  while IFS= read -r file; do
    psql_migrate "$file"
  done < <(ls "${MIGRATIONS_DIR}"/*.sql 2>/dev/null | grep -v '\.down\.sql$' | sort)

  echo "[migrations] done"
)

main "$@"
