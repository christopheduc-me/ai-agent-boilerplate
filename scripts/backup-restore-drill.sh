#!/usr/bin/env bash
# Backup restore drill (ADR-069): proves the documented `pg_dump` backup can
# actually be restored, and that the pgvector schema survives the round trip.
#
# A backup nobody has restored is not a backup — and this schema has two ways to
# fail that a plain PostgreSQL dump does not: the `vector` extension must exist
# on the target, and the `vector(768)` column plus its HNSW index must come back
# intact (ADR-063).
#
# Usage:
#   scripts/backup-restore-drill.sh                  # dump the compose database, restore, verify
#   scripts/backup-restore-drill.sh backup.sql       # verify an existing dump instead
#
# Exit code is the verdict: 0 = the backup restores and matches the source.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Must match the compose service, and must not be OLDER than it: pg_dump emits
# `\restrict` (a recent security hardening), and a psql that predates it aborts
# on line 5 with `invalid command \restrict`, restoring nothing. A stock
# `postgres:16` was measured failing exactly this way against a 16.14 dump.
IMAGE="${DRILL_IMAGE:-pgvector/pgvector:pg16}"
CONTAINER="aiagent-restore-drill-$$"
WORK="$(mktemp -d)"
DUMP="${1:-$WORK/dump.sql}"

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

say() { printf '\n== %s\n' "$*"; }
fail() { printf 'DRILL FAILED: %s\n' "$*" >&2; exit 1; }

# --- 1. the backup, taken exactly as docs/COMMANDS.md §10 prescribes ---------
if [ $# -eq 0 ]; then
  say "dumping the compose database (pg_dump -U app aiagent)"
  (cd "$ROOT" && docker compose exec -T postgres pg_dump -U app aiagent) > "$DUMP"
else
  say "using the supplied dump: $DUMP"
  [ -s "$DUMP" ] || fail "$DUMP is missing or empty"
fi
printf '   %s bytes\n' "$(wc -c < "$DUMP" | tr -d ' ')"

# Source counts, to compare against after the restore. Skipped when a dump file
# was supplied, since the live database may have moved on since it was taken.
SOURCE_COUNTS=""
if [ $# -eq 0 ]; then
  SOURCE_COUNTS="$(cd "$ROOT" && docker compose exec -T postgres psql -U app -d aiagent -tAc "
    select 'users=' || count(*) from users;
    select 'documents=' || count(*) from documents;
    select 'chunks=' || count(*) from document_chunks;")"
fi

# --- 2. restore into a throwaway instance -----------------------------------
say "restoring into a fresh $IMAGE"
docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$CONTAINER" \
  -e POSTGRES_USER=app -e POSTGRES_PASSWORD=app -e POSTGRES_DB=aiagent \
  "$IMAGE" >/dev/null
until docker exec "$CONTAINER" pg_isready -U app >/dev/null 2>&1; do sleep 1; done

# ON_ERROR_STOP turns a partial restore into a failure. Without it psql reports
# success while having skipped every statement it choked on.
if ! docker exec -i "$CONTAINER" psql -U app -d aiagent -v ON_ERROR_STOP=1 \
     < "$DUMP" > "$WORK/restore.log" 2>&1; then
  printf '\n--- first errors ---\n' >&2
  grep -iE '^(ERROR|invalid command)' "$WORK/restore.log" | head -5 >&2
  fail "psql refused the dump (see above). If it names an unknown command such as
  \\restrict, the target image is OLDER than the pg_dump that produced the backup —
  restore onto an image at least as new as the source."
fi

# --- 3. verify the schema this project actually depends on ------------------
say "verifying the restored database"
check() { # <label> <sql> <expected>
  local got
  got="$(docker exec "$CONTAINER" psql -U app -d aiagent -tAc "$2" | tr -d '[:space:]')"
  if [ "$got" = "$3" ]; then
    printf '   ok   %-22s %s\n' "$1" "$got"
  else
    printf '   FAIL %-22s got %s, expected %s\n' "$1" "$got" "$3"
    return 1
  fi
}

EXPECTED_MIGRATIONS="$(find "$ROOT/backend/migrations" -name '*.sql' | wc -l | tr -d ' ')"
rc=0
check "migrations" "select count(*) from _sqlx_migrations" "$EXPECTED_MIGRATIONS" || rc=1
check "vector extension" \
  "select count(*) from pg_extension where extname='vector'" "1" || rc=1
# The embedding column and its index are what a naive dump/restore loses.
check "embedding column" \
  "select format_type(atttypid, atttypmod) from pg_attribute
     where attrelid='document_chunks'::regclass and attname='embedding'" "vector(768)" || rc=1
check "hnsw index" \
  "select count(*) from pg_indexes
     where tablename='document_chunks' and indexdef ilike '%hnsw%'" "1" || rc=1
[ "$rc" -eq 0 ] || fail "the restored schema does not match the migrations"

# A structurally correct restore can still hold unusable vectors: run a real
# cosine search so the drill exercises the operator and the index, not just DDL.
say "running a vector similarity query against the restored data"
docker exec "$CONTAINER" psql -U app -d aiagent -tAc \
  "select count(*) from (
     select id from document_chunks
     order by embedding <=> (select embedding from document_chunks limit 1)
     limit 5) t" >/dev/null \
  || fail "cosine search failed on the restored data — the vectors did not survive"
printf '   ok   cosine search\n'

# --- 4. row counts must match the source ------------------------------------
if [ -n "$SOURCE_COUNTS" ]; then
  say "comparing row counts with the source"
  restored="$(docker exec "$CONTAINER" psql -U app -d aiagent -tAc "
    select 'users=' || count(*) from users;
    select 'documents=' || count(*) from documents;
    select 'chunks=' || count(*) from document_chunks;")"
  if [ "$(echo "$SOURCE_COUNTS" | tr -d '[:space:]')" != "$(echo "$restored" | tr -d '[:space:]')" ]; then
    printf 'source:   %s\nrestored: %s\n' "$SOURCE_COUNTS" "$restored" >&2
    fail "row counts differ between the source and the restore"
  fi
  while IFS= read -r line; do printf '   ok   %s\n' "$line"; done <<< "$restored"
fi

say "DRILL PASSED — this backup restores, and the pgvector schema survives it"
