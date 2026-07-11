#!/usr/bin/env bash
# End-to-end smoke test (ADR-021) against the full compose stack running with
# AGENT_PROVIDERS=fake. Exercises the real user journey through the nginx
# proxy: register -> login -> launch a search -> worker processes it -> results
# come back sorted by publication date.
#
# Usage: scripts/e2e-smoke.sh [BASE_URL]   (default: http://localhost:8080)
set -euo pipefail

BASE_URL="${1:-http://localhost:8080}"
EMAIL="e2e-$(date +%s)-$RANDOM@test.dev"
PASSWORD="e2e-s3cret-password"

say() { printf '\n== %s\n' "$*"; }
fail() { printf 'E2E FAILED: %s\n' "$*" >&2; exit 1; }

json_get() { # json_get <json> <python-expr on data>
  python3 -c "import json,sys; data=json.loads(sys.argv[1]); print($2)" "$1"
}

say "waiting for the stack ($BASE_URL)"
for _ in $(seq 1 30); do
  if curl -sf "$BASE_URL/api/../healthz" -o /dev/null 2>/dev/null \
     || curl -sf "$BASE_URL" -o /dev/null 2>/dev/null; then
    break
  fi
  sleep 2
done
curl -sf "$BASE_URL" -o /dev/null || fail "frontend unreachable at $BASE_URL"

say "register $EMAIL"
curl -sf -X POST "$BASE_URL/api/auth/register" \
  -H 'content-type: application/json' \
  -d "{\"email\":\"$EMAIL\",\"password\":\"$PASSWORD\"}" >/dev/null \
  || fail "register"

say "login"
LOGIN=$(curl -sf -X POST "$BASE_URL/api/auth/login" \
  -H 'content-type: application/json' \
  -d "{\"email\":\"$EMAIL\",\"password\":\"$PASSWORD\"}") || fail "login"
TOKEN=$(json_get "$LOGIN" 'data["access_token"]')
[ -n "$TOKEN" ] || fail "no access token in login response"

say "launch a search"
LAUNCH=$(curl -sf -X POST "$BASE_URL/api/searches" \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"keyword":"e2e smoke"}') || fail "launch search"
JOB_ID=$(json_get "$LAUNCH" 'data["job_id"]')
say "job $JOB_ID accepted, polling until completion"

STATUS="pending"
for _ in $(seq 1 30); do
  DETAIL=$(curl -sf "$BASE_URL/api/searches/$JOB_ID" \
    -H "authorization: Bearer $TOKEN") || fail "get search"
  STATUS=$(json_get "$DETAIL" 'data["status"]')
  [ "$STATUS" = "completed" ] && break
  [ "$STATUS" = "failed" ] && fail "job failed: $(json_get "$DETAIL" 'data["error"]')"
  sleep 2
done
[ "$STATUS" = "completed" ] || fail "job still '$STATUS' after timeout"

say "checking results (sorted, full date cascade)"
TITLES=$(json_get "$DETAIL" '",".join(r["title"] for r in data["results"])')
EXPECTED="fake-dated-recent,fake-llm-datable,fake-dated-old,fake-undatable"
[ "$TITLES" = "$EXPECTED" ] \
  || fail "unexpected result order: got [$TITLES], expected [$EXPECTED]"
CONFIDENCES=$(json_get "$DETAIL" '",".join(r["date_confidence"] for r in data["results"])')
[ "$CONFIDENCES" = "high,medium,high,unknown" ] \
  || fail "unexpected confidences: $CONFIDENCES"
# Timeline enrichment (ADR-027): event type + summary flow end to end.
EVENT_TYPES=$(json_get "$DETAIL" '",".join(r["event_type"] for r in data["results"])')
[ "$EVENT_TYPES" = "announcement,announcement,announcement,announcement" ] \
  || fail "unexpected event types: $EVENT_TYPES"
FIRST_SUMMARY=$(json_get "$DETAIL" 'data["results"][0]["summary"]')
[ "$FIRST_SUMMARY" = "Fake summary for fake-dated-recent" ] \
  || fail "unexpected summary: $FIRST_SUMMARY"

say "E2E OK — status=$STATUS, results=[$TITLES]"
