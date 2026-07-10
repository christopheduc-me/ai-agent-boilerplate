# Cross-language contract fixtures (ADR-025)

Golden examples of the internal JSON contracts between the Rust backend and
the Python agent (ARCHITECTURE.md §4). Each fixture is asserted on **both
sides** of the wire, in the direction of the real traffic:

| Fixture | Producer (must serialize exactly this) | Consumer (must parse this) |
|---|---|---|
| `task-request.json` | backend (`HttpJobDispatcher`) | agent (FastAPI `TaskRequest`) |
| `results-callback.json` | agent (`serialize_result`) | backend (`POST /internal/jobs/{id}/results`) |
| `failure-callback.json` | agent (`report_failure`) | backend (`POST /internal/jobs/{id}/failure`) |

Tests: `backend/tests/contract.rs` and `agent/tests/test_contract.py`.
A drift in either language breaks one of the two suites instead of surfacing
in the e2e test (or production).
