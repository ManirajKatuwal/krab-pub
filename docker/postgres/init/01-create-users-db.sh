#!/usr/bin/env bash

krab_create_users_db() {
  local users_db="${POSTGRES_DB_USERS:-}"
  local primary_db="${POSTGRES_DB:-}"

  if [ -z "$users_db" ] || [ "$users_db" = "$primary_db" ]; then
    return 0
  fi

  psql -v ON_ERROR_STOP=1 \
    --username "$POSTGRES_USER" \
    --dbname "$primary_db" \
    --set=users_db="$users_db" <<'SQL'
SELECT format('CREATE DATABASE %I', :'users_db')
WHERE NOT EXISTS (
  SELECT 1
  FROM pg_database
  WHERE datname = :'users_db'
)\gexec
SQL
}

krab_create_users_db && unset -f krab_create_users_db
