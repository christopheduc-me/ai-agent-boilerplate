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
      (`RATE_LIMIT_AUTH_PER_MINUTE`, `RATE_LIMIT_API_PER_MINUTE`). Per-account
      login throttle (ADR-057, `LOGIN_MAX_ATTEMPTS_PER_MINUTE`) caps IP-rotating
      credential-stuffing, plus an append-only security audit log
      (`security_events`, migration 0010; failed/throttled logins, refresh reuse,
      quota hits) purged after `SECURITY_EVENT_RETENTION_DAYS`.
- [x] **Refresh tokens (ADR-008)** — done: single-use rotation on `/refresh`,
      SHA-256-hashed storage (migration 0002), HttpOnly cookie scoped to
      `/api/auth`, revocation on `/logout`, expired-token purge by the reaper.
      Reuse detection + family revocation (ADR-056, migration 0009): replaying a
      consumed token revokes the whole login lineage. Frontend: silent session
      restore on reload + refresh-and-retry on 401 (`withAuth`), redirect to
      login when the session is gone.

## P2 — Operability

- [x] **End-to-end correlation (ADR-018)** — done: `X-Request-Id` middleware on
      the Rust API, `job_id` propagated Rust → FastAPI → Celery → callbacks,
      `LOG_FORMAT=json` structured logs on all three server processes
      (enabled in `deploy/docker-compose.prod.yml`).
- [x] **Security hygiene in CI (ADR-015 amendment)** — done: `audit` stage with
      `cargo audit`, `pip-audit`, `npm audit`, gitleaks; runs on the weekly
      schedule only (creation of the schedule: SETUP.md §3).
- [x] **Security-event metrics (ADR-060)** — done: a `MeteredSecurityAudit`
      decorator over the audit port emits a `security.events` counter (by kind),
      so failed/throttled logins, refresh reuse and quota hits are alertable in
      Prometheus/Grafana (PromQL in docs/OBSERVABILITY.md).
- [x] **Data lifecycle (ADR-058)** — done: background-loop retention purge of
      finished one-shot searches (`DATA_RETENTION_DAYS`, opt-in; recurring-run
      history spared as ADR-033 dedup memory), and account deletion
      (`DELETE /api/account`) erasing the user's jobs/results/recurring/tokens
      through the ports (cascade as safety net).

## P2.5 — Agentic capabilities (ADR-030 follow-ups)

- [x] **Agentic loop + live decision journal (ADR-030)** — done: `mode=agent`
      end to end, `AgentPolicy`/`StepReporter` ports, step budget
      (`AGENT_MAX_STEPS`), `agent_steps` journal streamed over SSE, two demo
      blocks in the frontend.
- [x] **Result self-critique (ADR-031)** — done: a `ResultCritic` port reviews
      the hits before delivery (verdict journaled as a `critique` step,
      off-topic URLs dropped, at most one budget-bounded repair search).
- [x] **Recurring searches with memory (ADR-033)** — done: saved searches
      re-run by the backend scheduler tick (Celery beat rejected — see the
      ADR), `seen_urls` memory, `is_new` flags end to end, and a `report`
      journal step with the delta verdict.
- [x] **Digest webhooks (ADR-036)** — done: optional `webhook_url` per
      recurring search; runs with new results POST a digest (best-effort,
      shape pinned by `contracts/digest-webhook.json`).
- [x] **Per-user notification channels (ADR-061/062)** — done: profile-level
      Slack, Telegram and Email destinations (`GET`/`POST`/`DELETE
      /api/account[/channels]`), delivered alongside the per-search webhook via a
      `ChannelNotifier` port (Slack reuses the SSRF guard; email over SMTP behind
      an `EmailTransport` port, opt-in via `SMTP_*`, gated by `email_enabled`).
- [x] **Human-in-the-loop clarification (ADR-032)** — done: the policy can ask
      one question (`awaiting_input` status, reaper-exempt), the answer
      re-dispatches the job with the clarification and a fresh journal.

## P3 — Agent product quality

- [x] **RAG knowledge base (ADR-063)** — done: per-user documents (text/markdown)
      uploaded, chunked + embedded by the agent (`EmbeddingProvider`: fake +
      Ollama), stored in pgvector, and the top-k chunks retrieved to ground the
      agent's reasoning. Backend owns the store (`/api/documents`, migration
      0013, `pgvector/pgvector` image); agent embeds + retrieves through the
      internal API (`/embed`, `/internal/documents/*`, `/internal/retrieve`).

- [x] **Date cascade stage 2 (ADR-035)** — done: `PageDateFetcher` port reads
      JSON-LD `datePublished` / OpenGraph `article:published_time` before the
      LLM fallback — `high` confidence, bounded fetch, silent degradation.
- [x] **URL normalization + deduplication (ADR-034)** — done: canonical URLs
      (tracking params, fragments, ports, param order) used for workflow and
      loop deduplication and for the ADR-033 memory matching; displayed URLs
      stay original.
- [x] **Opt-in live provider tests (ADR-012)** — done:
      `agent/tests/test_live_providers.py`, skipped unless `RUN_LIVE_TESTS=1`
      (never in CI). One test per paid adapter (Tavily search, Claude
      enricher/policy/critic) to catch provider drift that defensive parsing
      would degrade silently; validated once for real on 2026-07-17.

## P4 — Comfort (later)

- [x] **E2E smoke test on the full compose stack in CI (ADR-021)** — done:
      deterministic fake providers (`AGENT_PROVIDERS=fake`, keyless),
      `scripts/e2e-smoke.sh` through nginx, `e2e` job in GitHub Actions and
      the GitLab mirror.
- [x] **Browser-level e2e tests (Playwright, ADR-028)** — done: real Chromium
      journeys (register → search → timeline, re-login → history) against the
      same fake-provider stack, run by both CIs' `e2e` jobs.
- [x] **Opt-in OpenTelemetry observability (ADR-029/050)** — done: gated on
      `OTEL_EXPORTER_OTLP_ENDPOINT`, W3C context propagated backend → FastAPI
      → Celery → callbacks. All three pillars behind `--profile observability`:
      traces (Jaeger) with per-call LLM spans, structured logs carrying the
      `trace_id`, and metrics (OTel Collector → Prometheus → Grafana — LLM
      latency/tokens/cost, HTTP RED).
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
- [x] **Readiness probe (ADR-059)** — done: `GET /readyz` (DB `SELECT 1` behind
      a `ReadinessProbe` port) returns 503 while the database is unreachable, so
      a load balancer / orchestrator drains the instance without tripping the
      `/healthz` liveness probe.
- [x] **Cross-language contract fixtures (ADR-025)** — done: `contracts/`
      golden files asserted by both the Rust and Python suites.
- [x] **Trivy image scanning (ADR-015 amendment)** — done: weekly HIGH/CRITICAL
      CVE scan of the three published images in both CIs.
- [x] **Distributed per-IP rate limiting (ADR-037, revisits ADR-017)** — done
      as an opt-in: `RATE_LIMIT_REDIS_URL` switches the middleware to a
      Redis-shared fixed window (fail-open on Redis outages); unset keeps the
      in-memory limiter. Rate limiting at the reverse proxy remains the
      zero-code alternative for fleets behind a shared proxy tier.
- [ ] **Alertmanager for the observability profile (ADR-068 follow-up)** — the
      six starter rules in `deploy/observability/alerts.yml` decide *when*
      something fires; nothing routes it. Until a fork adds Alertmanager they
      surface at `:9090/alerts` or back a Grafana alert, which means an incident
      at 3am waits for someone to look. Adding it is a service in the
      `observability` profile plus three decisions the boilerplate cannot make
      for you: which channel, which rotation, and which inhibition rules
      (`CollectorScrapeDown` already carries a `blackhole` label for exactly
      that — while the pipeline is blind, every other rule's silence is
      meaningless). Tune the thresholds first: routing untuned alerts just
      pages people about nothing.
- [ ] **Deep Agents as the orchestrator (ADR-065)** — evaluated and deferred, not
      rejected. `deepagents` is an opinionated harness above LangGraph adding a
      planning tool, subagents with their own context, and a filesystem backend
      for context offloading. The agent mode (four nodes, three typed actions)
      does not need any of them, and the harness hands the loop to the model,
      which weakens the step budget (ADR-030), defensive parsing (ADR-043) and
      the spend cap (ADR-048). **Trigger to revisit**: a long-deliverable use
      case (a 200-question questionnaire, a dossier analysis, a tender response)
      — there all three earn their keep. Answer first whether it can honour an
      external spend cap and emit typed journal steps; that decides whether it
      is an adapter swap or a rewrite of the guardrails. Middle path if only
      part is wanted: `Planner` / `SubAgent` domain ports, added the way
      `AgentPolicy` and `ResultCritic` were. See ADR-065.
- [ ] **TypeScript 7 for the frontend (ADR-064 follow-up)** — blocked upstream,
      not by a stale pin. TS 7 is the native (Go) port and its npm package drops
      the JavaScript compiler API; both `vue-tsc` 3.3.9 and `typescript-eslint`
      8.65.0 — each the newest release — still need it, the latter declaring
      `typescript: >=4.8.4 <6.1.0` and failing hard with
      `typescript-eslint does not support TS 7.0`. The frontend therefore stays
      on **typescript 6.0.3**. TypeScript **7.1** is the announced release that
      ships the stable programmatic API those tools need (expected ~October
      2026); Vue, Svelte, Astro and MDX are all waiting on it. Recheck then —
      it should be a plain version bump of `typescript`, `vue-tsc` and
      `@vue/eslint-config-typescript`, with `npm run typecheck` and
      `npm run lint` as the acceptance test. Tracking:
      https://github.com/vuejs/language-tools/issues/5381
      Rewriting the 8 SFCs as TSX (`jsx: preserve`, `jsxImportSource: vue`)
      would unblock TS 7 today, but that is a frontend architecture change —
      losing `<template>`, scoped styles and `v-model` sugar — not a version
      bump, and it was rejected on those grounds.
