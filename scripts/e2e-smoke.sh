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

say "launch an agent-mode search (ADR-030)"
LAUNCH=$(curl -sf -X POST "$BASE_URL/api/searches" \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"keyword":"e2e agent smoke","mode":"agent"}') || fail "launch agent search"
AGENT_JOB=$(json_get "$LAUNCH" 'data["job_id"]')

AGENT_STATUS="pending"
for _ in $(seq 1 30); do
  AGENT_DETAIL=$(curl -sf "$BASE_URL/api/searches/$AGENT_JOB" \
    -H "authorization: Bearer $TOKEN") || fail "get agent search"
  AGENT_STATUS=$(json_get "$AGENT_DETAIL" 'data["status"]')
  [ "$AGENT_STATUS" = "completed" ] && break
  [ "$AGENT_STATUS" = "failed" ] && fail "agent job failed"
  sleep 2
done
[ "$AGENT_STATUS" = "completed" ] || fail "agent job still '$AGENT_STATUS'"

say "checking the agent decision journal"
MODE=$(json_get "$AGENT_DETAIL" 'data["mode"]')
[ "$MODE" = "agent" ] || fail "unexpected mode: $MODE"
STEP_KINDS=$(json_get "$AGENT_DETAIL" '",".join(s["kind"] for s in data["steps"])')
# Fake policy + critic (ADR-030/031): search -> refine (deduplicated to 0 new)
# -> reasoned finish -> self-critique review.
[ "$STEP_KINDS" = "search,search,finish,critique" ] || fail "unexpected steps: $STEP_KINDS"
NEW_HITS=$(json_get "$AGENT_DETAIL" '",".join(str(s["new_hits"]) for s in data["steps"])')
[ "$NEW_HITS" = "4,0,0,0" ] || fail "unexpected new_hits: $NEW_HITS"
CRITIQUE=$(json_get "$AGENT_DETAIL" 'data["steps"][-1]["reason"]')
case "$CRITIQUE" in
  "All 4 results relate to the goal"*) ;;
  *) fail "unexpected critique reason: $CRITIQUE" ;;
esac
AGENT_RESULTS=$(json_get "$AGENT_DETAIL" 'len(data["results"])')
[ "$AGENT_RESULTS" = "4" ] || fail "unexpected agent result count: $AGENT_RESULTS"

say "E2E OK — workflow results=[$TITLES]; agent steps=[$STEP_KINDS]"
