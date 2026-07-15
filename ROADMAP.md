# ROADMAP — technical roadmap and ideas

Deliberate scope cuts, hardening steps, and ideas for the boilerplate, ordered
by risk. The ports are in place — each item is an adapter/use-case cycle away.
Manual setup and deployment steps live in [SETUP.md](SETUP.md).

## P1 — Core reliability (before real usage)

- [x] **PostgreSQL adapter** (sqlx, ADR-007) — done. PostgreSQL whenever
      `DATABASE_URL` is set (migrations at startup), in-memory fallback
      otherwise; integration tests against compose locally / GitLab service in CI.
- [x] **Job lifecycle robustness (ADR-016)** — done: `running` transition,
      backend reaper (`JOB_TIMEOUT_MINUTES`), Celery retries with backoff,
      idempotent end to end.
- [x] **Rate limiting + quotas (ADR-017)** — done: per-user daily search quota
      (`DAILY_SEARCH_QUOTA`), per-IP fixed-window limits on auth and API routes
      (`RATE_LIMIT_AUTH_PER_MINUTE`, `RATE_LIMIT_API_PER_MINUTE`).
- [x] **Refresh tokens (ADR-008)** — done: single-use rotation on `/refresh`,
      SHA-256-hashed storage (migration 0002), HttpOnly cookie scoped to
      `/api/auth`, revocation on `/logout`, expired-token purge by the reaper.
      Frontend: silent session restore on reload + refresh-and-retry on 401
      (`withAuth`), redirect to login when the session is gone.

## P2 — Operability

- [x] **End-to-end correlation (ADR-018)** — done: `X-Request-Id` middleware on
      the Rust API, `job_id` propagated Rust → FastAPI → Celery → callbacks,
      `LOG_FORMAT=json` structured logs on all three server processes
      (enabled in `deploy/docker-compose.prod.yml`).
- [x] **Security hygiene in CI (ADR-015 amendment)** — done: `audit` stage with
      `cargo audit`, `pip-audit`, `npm audit`, gitleaks; runs on the weekly
      schedule only (creation of the schedule: SETUP.md §3).

## P2.5 — Agentic capabilities (ADR-030 follow-ups)

- [x] **Agentic loop + live decision journal (ADR-030)** — done: `mode=agent`
      end to end, `AgentPolicy`/`StepReporter` ports, step budget
      (`AGENT_MAX_STEPS`), `agent_steps` journal streamed over SSE, two demo
      blocks in the frontend.
- [x] **Result self-critique (ADR-031)** — done: a `ResultCritic` port reviews
      the hits before delivery (verdict journaled as a `critique` step,
      off-topic URLs dropped, at most one budget-bounded repair search).
- [ ] **Recurring searches with memory**: saved searches re-run by Celery
      beat; the agent compares against previously seen URLs and decides
      whether the delta is worth reporting.
- [ ] **Human-in-the-loop clarification**: an `awaiting_input` job status —
      the policy can ask the user a question (ambiguous goal), the job pauses,
      the answer resumes the loop (SSE already streams the state).

## P3 — Agent product quality

- [ ] **Date cascade stage 2 (ADR-011)**: fetch the page and read JSON-LD
      `datePublished` / OpenGraph before falling back to the LLM — cheaper and
      `high` confidence instead of `medium`.
- [ ] **URL normalization + deduplication** in the agent domain (tracking params
      make the same article count twice today).
- [ ] Optional `RUN_LIVE_TESTS=1` integration tests for the Tavily and Claude
      adapters (ADR-012).

## P4 — Comfort (later)

- [x] **E2E smoke test on the full compose stack in CI (ADR-021)** — done:
      deterministic fake providers (`AGENT_PROVIDERS=fake`, keyless),
      `scripts/e2e-smoke.sh` through nginx, `e2e` job in GitHub Actions and
      the GitLab mirror.
- [x] **Browser-level e2e tests (Playwright, ADR-028)** — done: real Chromium
      journeys (register → search → timeline, re-login → history) against the
      same fake-provider stack, run by both CIs' `e2e` jobs.
- [x] **Opt-in OpenTelemetry traces (ADR-029)** — done: gated on
      `OTEL_EXPORTER_OTLP_ENDPOINT`, W3C context propagated backend → FastAPI
      → Celery → callbacks, dev Jaeger behind `--profile observability`.
      Metrics/logs export could follow the same pattern if a fork needs it.
- [x] **Dependency freshness without a platform bot (ADR-022)** — done:
      `scripts/deps-report.sh` (native tools) run weekly by both CIs, plus an
      inert portable `renovate.json` for forks that want automated update PRs
      (connect the Mend app on GitHub, or a scheduled renovate container job
      on GitLab/self-hosted, to activate it).
- [x] **Live job updates over SSE (ADR-026)** — done: `GET
      /api/searches/{id}/events` (DB-poll stream, closes on terminal status),
      fetch-streaming client with automatic polling fallback.
- [x] **Code coverage reporting in CI (ADR-023)** — done: cargo llvm-cov /
      pytest-cov / vitest v8 in the test jobs, Codecov on GitHub (informational,
      per-brick flags), native `coverage:` regex on the GitLab mirror.
- [x] **Pre-commit hooks (lefthook, ADR-022 amendment)** — done: fast
      format/lint per brick + gitleaks staged scan; `lefthook install` to opt in.
- [x] **Graceful shutdown of the backend (ADR-024)** — done: SIGTERM/SIGINT
      drain via `with_graceful_shutdown`.
- [x] **Cross-language contract fixtures (ADR-025)** — done: `contracts/`
      golden files asserted by both the Rust and Python suites.
- [x] **Trivy image scanning (ADR-015 amendment)** — done: weekly HIGH/CRITICAL
      CVE scan of the three published images in both CIs.
- [ ] Distributed per-IP rate limiting **if** the backend ever scales
      horizontally (ADR-017). Note: the per-user quota is already
      multi-instance-safe (it counts rows in PostgreSQL); only the in-memory
      IP limiter is per-instance, and its degradation is benign (effective
      limit becomes N× the configured one). When needed, prefer rate limiting
      at the reverse proxy/load balancer (zero app code) over a Redis-backed
      limiter — the latter only pays off for fine-grained per-user rules. The
      swap surface is a single file (`backend/src/adapters/http/rate_limit.rs`).
