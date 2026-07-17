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
EXPECTED="fake-dated-recent,fake-page-datable,fake-llm-datable,fake-dated-old,fake-undatable"
[ "$TITLES" = "$EXPECTED" ] \
  || fail "unexpected result order: got [$TITLES], expected [$EXPECTED]"
# Full date cascade (ADR-011/035): provider high, page-declared high,
# LLM medium, unknown.
CONFIDENCES=$(json_get "$DETAIL" '",".join(r["date_confidence"] for r in data["results"])')
[ "$CONFIDENCES" = "high,high,medium,high,unknown" ] \
  || fail "unexpected confidences: $CONFIDENCES"
# Timeline enrichment (ADR-027): event type + summary flow end to end.
EVENT_TYPES=$(json_get "$DETAIL" '",".join(r["event_type"] for r in data["results"])')
[ "$EVENT_TYPES" = "announcement,announcement,announcement,announcement,announcement" ] \
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

say "checking the workflow run's usage (ADR-038)"
# Fake mode: calls are counted, cost is $0 — 5 enricher calls, 1 search.
USAGE=$(json_get "$DETAIL" '"{}/{}/{}".format(data["usage"]["llm_calls"], data["usage"]["search_calls"], data["usage"]["cost_usd"])')
[ "$USAGE" = "5/1/0.0" ] || fail "unexpected workflow usage: $USAGE"

say "checking the agent decision journal"
MODE=$(json_get "$AGENT_DETAIL" 'data["mode"]')
[ "$MODE" = "agent" ] || fail "unexpected mode: $MODE"
STEP_KINDS=$(json_get "$AGENT_DETAIL" '",".join(s["kind"] for s in data["steps"])')
# Fake policy + critic (ADR-030/031): search -> refine (deduplicated to 0 new)
# -> reasoned finish -> self-critique review.
[ "$STEP_KINDS" = "search,search,finish,critique" ] || fail "unexpected steps: $STEP_KINDS"
NEW_HITS=$(json_get "$AGENT_DETAIL" '",".join(str(s["new_hits"]) for s in data["steps"])')
[ "$NEW_HITS" = "5,0,0,0" ] || fail "unexpected new_hits: $NEW_HITS"
CRITIQUE=$(json_get "$AGENT_DETAIL" 'data["steps"][-1]["reason"]')
case "$CRITIQUE" in
  "All 5 results relate to the goal"*) ;;
  *) fail "unexpected critique reason: $CRITIQUE" ;;
esac
AGENT_RESULTS=$(json_get "$AGENT_DETAIL" 'len(data["results"])')
[ "$AGENT_RESULTS" = "5" ] || fail "unexpected agent result count: $AGENT_RESULTS"
# Agent-mode usage (ADR-038): enricher x5 + policy x3 + critic x1, 2 searches.
AGENT_USAGE=$(json_get "$AGENT_DETAIL" '"{}/{}/{}".format(data["usage"]["llm_calls"], data["usage"]["search_calls"], data["usage"]["cost_usd"])')
[ "$AGENT_USAGE" = "9/2/0.0" ] || fail "unexpected agent usage: $AGENT_USAGE"

say "launch an ambiguous agent search (HITL, ADR-032)"
HITL=$(curl -sf -X POST "$BASE_URL/api/searches" \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"keyword":"ambiguous smoke topic","mode":"agent"}') || fail "launch HITL search"
HITL_ID=$(json_get "$HITL" 'data["job_id"]')

HITL_STATUS="pending"
for _ in $(seq 1 30); do
  HITL_DETAIL=$(curl -sf "$BASE_URL/api/searches/$HITL_ID" \
    -H "authorization: Bearer $TOKEN") || fail "get HITL search"
  HITL_STATUS=$(json_get "$HITL_DETAIL" 'data["status"]')
  [ "$HITL_STATUS" = "awaiting_input" ] && break
  [ "$HITL_STATUS" = "failed" ] && fail "HITL job failed"
  sleep 2
done
[ "$HITL_STATUS" = "awaiting_input" ] || fail "HITL job never paused (status: $HITL_STATUS)"
QUESTION=$(json_get "$HITL_DETAIL" 'data["question"]')
case "$QUESTION" in
  "Your goal looks ambiguous"*) ;;
  *) fail "unexpected question: $QUESTION" ;;
esac

say "answer the clarification and wait for completion"
curl -sf -X POST "$BASE_URL/api/searches/$HITL_ID/answer" \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"answer":"the cars"}' >/dev/null || fail "answer clarification"
for _ in $(seq 1 30); do
  HITL_DETAIL=$(curl -sf "$BASE_URL/api/searches/$HITL_ID" \
    -H "authorization: Bearer $TOKEN") || fail "get HITL search"
  HITL_STATUS=$(json_get "$HITL_DETAIL" 'data["status"]')
  [ "$HITL_STATUS" = "completed" ] && break
  sleep 2
done
[ "$HITL_STATUS" = "completed" ] || fail "HITL job still '$HITL_STATUS' after the answer"
[ "$(json_get "$HITL_DETAIL" 'data["answer"]')" = "the cars" ] || fail "answer not stored"
[ "$(json_get "$HITL_DETAIL" 'len(data["results"])')" = "5" ] || fail "HITL results missing"
HITL_KINDS=$(json_get "$HITL_DETAIL" '",".join(s["kind"] for s in data["steps"])')
# Fresh journal after the resume (replace semantics): the full loop + critique.
[ "$HITL_KINDS" = "search,search,finish,critique" ] || fail "unexpected HITL steps: $HITL_KINDS"

say "create a recurring search (ADR-033) — the scheduler launches the first run"
REC=$(curl -sf -X POST "$BASE_URL/api/recurring" \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"keyword":"recurring smoke","mode":"agent","interval_minutes":1}') || fail "create recurring"
REC_ID=$(json_get "$REC" 'data["id"]')

completed_runs() { # -> newline-separated job ids, oldest first
  LIST=$(curl -sf "$BASE_URL/api/searches" -H "authorization: Bearer $TOKEN") || fail "list searches"
  json_get "$LIST" "'\n'.join(j['id'] for j in reversed(data) if j.get('recurring_search_id') == '$REC_ID' and j['status'] == 'completed')"
}

# First run: due immediately; the e2e stack ticks every SCHEDULER_TICK_SECONDS=5.
RUN1_ID=""
for _ in $(seq 1 30); do
  RUN1_ID=$(completed_runs | sed -n '1p')
  [ -n "$RUN1_ID" ] && break
  sleep 2
done
[ -n "$RUN1_ID" ] || fail "the scheduler never launched the first recurring run"
RUN1=$(curl -sf "$BASE_URL/api/searches/$RUN1_ID" -H "authorization: Bearer $TOKEN")
[ "$(json_get "$RUN1" 'all(r["is_new"] for r in data["results"])')" = "True" ] \
  || fail "first recurring run: everything should be new"
[ "$(json_get "$RUN1" 'data["steps"][-1]["kind"]')" = "report" ] || fail "missing report step"
[ "$(json_get "$RUN1" 'data["steps"][-1]["new_hits"]')" = "5" ] || fail "first run should report 5 new"

say "wait for the second run — the memory flags everything as already seen"
RUN2_ID=""
for _ in $(seq 1 60); do
  RUN2_ID=$(completed_runs | sed -n '2p')
  [ -n "$RUN2_ID" ] && break
  sleep 2
done
[ -n "$RUN2_ID" ] || fail "the scheduler never launched the second recurring run"
RUN2=$(curl -sf "$BASE_URL/api/searches/$RUN2_ID" -H "authorization: Bearer $TOKEN")
[ "$(json_get "$RUN2" 'any(r["is_new"] for r in data["results"])')" = "False" ] \
  || fail "second recurring run: nothing should be new"
REPORT=$(json_get "$RUN2" 'data["steps"][-1]["reason"]')
[ "$REPORT" = "Nothing new since the last run" ] || fail "unexpected report: $REPORT"

curl -sf -X DELETE "$BASE_URL/api/recurring/$REC_ID" \
  -H "authorization: Bearer $TOKEN" >/dev/null || fail "delete recurring"

say "E2E OK — workflow results=[$TITLES]; agent steps=[$STEP_KINDS]; HITL answered; recurring delta verified"
