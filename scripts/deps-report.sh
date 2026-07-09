#!/usr/bin/env bash
# Outdated-dependencies report (ADR-022): native package managers only — no
# bot, no platform-specific service. Informative (never fails the pipeline):
# it tells you what an upgrade would change; applying it stays a human action
# (see docs/COMMANDS.md §9).
#
# Usage: scripts/deps-report.sh [backend|agent|frontend|all]   (default: all)
set -euo pipefail

COMPONENT="${1:-all}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

section() { printf '\n===== %s =====\n' "$*"; }

report_backend() {
  section "backend — cargo update --dry-run"
  (cd "$ROOT/backend" && cargo update --dry-run 2>&1) \
    | grep -Ev "^\s*(Updating .* index|Locking|note:)" || true
}

report_agent() {
  section "agent — uv lock --upgrade --dry-run"
  (cd "$ROOT/agent" && uv lock --upgrade --dry-run 2>&1) || true
}

report_frontend() {
  section "frontend — npm outdated"
  # npm outdated exits 1 whenever something is outdated: informative, not an error.
  (cd "$ROOT/frontend" && npm outdated) || true
}

case "$COMPONENT" in
  backend) report_backend ;;
  agent) report_agent ;;
  frontend) report_frontend ;;
  all)
    report_backend
    report_agent
    report_frontend
    ;;
  *)
    echo "usage: $0 [backend|agent|frontend|all]" >&2
    exit 2
    ;;
esac

printf '\nDone. To apply upgrades, see docs/COMMANDS.md §9 (then run the test suites).\n'
