# Cross-language contract fixtures (ADR-025 / ADR-049)

Golden examples of the JSON contracts crossing a language boundary
(ARCHITECTURE.md §4). Each fixture is asserted on **both sides** of the wire,
in the direction of the real traffic, so a drift in either language breaks one
of the suites instead of surfacing in the e2e test (or production).

**Internal contract** — Rust backend ↔ Python agent (ADR-025):

| Fixture | Producer (must serialize exactly this) | Consumer (must parse this) |
|---|---|---|
| `task-request.json` | backend (`HttpJobDispatcher`) | agent (FastAPI `TaskRequest`) |
| `results-callback.json` | agent (`serialize_result`) | backend (`POST /internal/jobs/{id}/results`) |
| `failure-callback.json` | agent (`report_failure`) | backend (`POST /internal/jobs/{id}/failure`) |
| `agent-step-callback.json` | agent (`serialize_step`) | backend (`POST /internal/jobs/{id}/steps`) |
| `question-callback.json` | agent | backend (`POST /internal/jobs/{id}/question`) |
| `usage-callback.json` | agent (`serialize_usage`) | backend (`POST /internal/jobs/{id}/usage`) |
| `digest-webhook.json` | backend (`WebhookDigestSender`) | the user's systems (Slack, n8n…) |

**Public contract** — Rust backend → Vue frontend (ADR-049):

| Fixture | Producer (serializes exactly this) | Consumer (validates this) |
|---|---|---|
| `search-job-detail.json` | backend (`job_detail_json`, `GET /api/searches/{id}`) | frontend (`searchJobDetailSchema`) |
| `recurring-search.json` | backend (`recurring_search_json`, `/api/recurring`) | frontend (`recurringSearchSchema`) |

Tests: `backend/tests/contract.rs`, `agent/tests/test_contract.py`, and
`frontend/src/__tests__/contract.spec.ts`. The frontend validates with the
`zod` schemas that back `api.ts` (runtime validation, not just types) and
**tolerates unknown fields** — an additive backend change is stripped, not
rejected, so an older frontend keeps working during a rolling deploy (ADR-049).
