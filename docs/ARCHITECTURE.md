# Architecture Decisions — AI Agent Boilerplate

> Reference document for the project's technical decisions.
> Each decision is dated and motivated; if a decision is revisited, add a new
> entry rather than rewriting history.
>
> **Maintenance rule — this file is the single source of truth.** Any change
> that affects the architecture (new adapter, new dependency, changed contract,
> changed infrastructure, revisited decision) MUST update this document in the
> same commit. When reality and this document disagree, fixing the discrepancy
> is part of the change. Implementation gaps are marked *(planned)* here and
> tracked in `ROADMAP.md`.

Last updated: 2026-07-17

**Language convention**: all documentation, code, comments, commit messages, and
identifiers in this project are written in **English only**.

---

## 1. Project goal

Boilerplate for building AI agents:

- A user creates an account on a website.
- They enter a **keyword** and launch an AI agent.
- The agent searches the web for information about that keyword.
- Results are **ranked by publication date** of the source.

## 2. Overview

Three components in a monorepo:

```
aiagent_boilerplate/
├── backend/     # Rust / Axum — web API, accounts, job orchestration
├── agent/       # Python — FastAPI micro-API + Celery workers (LangChain)
├── frontend/    # Vue 3 / Vite — simple SPA
├── docs/        # This document + future ADRs
└── docker-compose.yml   # PostgreSQL, Redis, services
```

### Nominal flow

```
Vue (SPA)
  │  POST /api/searches {keyword}          (JWT)
  ▼
Axum (Rust) ── persists the job (PostgreSQL, status=pending)
  │  POST /tasks {job_id, keyword}         (internal HTTP)
  ▼
FastAPI (Python) ── enqueues via Celery
  │
  ▼
Redis (broker) ──▶ Celery worker ── LangChain agent
                        │   0. POST /internal/jobs/{id}/started (→ running)
                        │   1. web search (Tavily)
                        │   2. date extraction/normalization
                        │   3. sort by publication date
                        ▼
                   POST /internal/jobs/{id}/results  ──▶ Axum ── persists
                                                              ▲
Vue (polling GET /api/searches/{id}) ─────────────────────────┘
```

---

## 3. Decisions

### ADR-001 — Three-component monorepo

**Decision**: a single repository, three directories (`backend/`, `agent/`, `frontend/`).

**Why**: this is a boilerplate — its value is demonstrating end-to-end integration.
A monorepo simplifies cloning, consistent versioning of API contracts between
components, and a single docker-compose file.

### ADR-002 — Web backend: Rust / Axum, hexagonal architecture

**Decision**: Axum + Tokio. Strict hexagonal architecture:

```
backend/src/
├── domain/          # Entities (User, ResearchJob, SearchResult) + pure business logic
│   └── ports.rs     # Traits: UserRepository, JobRepository, JobDispatcher,
│                    #         PasswordHasher, TokenService
├── application/     # Use cases: RegisterUser, LoginUser, LaunchSearch,
│                    #            IngestResults, SearchQueries (read side)
└── adapters/
    ├── http/        # Axum handlers, extractors, DTOs (inbound adapter)
    ├── persistence/ # postgres (sqlx) + in_memory fallback (outbound adapter)
    ├── auth/        # argon2id hasher + JWT token service (outbound adapter)
    └── dispatch/    # HttpJobDispatcher → Python micro-API + noop fallback (outbound)
```

**Rules**:
- `domain` depends on no infrastructure crate (no axum, no sqlx, no reqwest).
- Ports are traits; adapters implement them.
- Use cases are unit-tested with fakes/mocks of the ports.

### ADR-003 — Frontend: Vue 3 + Vite (simple)

**Decision**: Vue 3 (Composition API, `<script setup>`), Vite, Pinia for state,
Vue Router. No heavy UI framework for now — plain CSS.

**Pages**: sign-up, login, keyword form, list of searches, search detail
(results sorted by date; sources without a date shown separately).

**Refresh**: live updates over SSE (ADR-026), with plain 2.5 s polling kept as
an automatic fallback when the stream fails.

### ADR-004 — AI agent: Python, LangChain + Celery + Redis, hexagonal too

**Decision**: Celery workers run the LangChain agent. The agent core is isolated
from Celery and from external providers:

```
agent/src/aiagent/
├── domain/
│   ├── models.py    # RawSearchHit, ResearchResult, date normalization, sorting
│   └── ports.py     # Protocols: SearchProvider, DateExtractor(LLM), ResultSink
├── application/
│   └── run_research.py  # pure orchestration (date cascade, sort, deliver)
├── adapters/
│   ├── tavily.py    # SearchProvider → Tavily
│   ├── llm.py       # DateExtractor → Claude via langchain-anthropic
│   ├── sink.py      # ResultSink → POST /internal/jobs/{id}/results (Rust API)
│   └── api/app.py   # FastAPI: POST /tasks (inbound adapter, see ADR-005)
├── celery_app.py    # Celery app (broker/result backend on Redis)
└── tasks.py         # Celery tasks: thin glue calling application/
```

**Rule**: `tasks.py` and the FastAPI app contain no business logic — they
instantiate adapters and call the use case.

### ADR-005 — Rust → Celery integration: Python micro-API (FastAPI)

**Decision**: the Rust backend does not write directly to Redis in the Celery
format. It calls a FastAPI micro-API (`POST /tasks`) which enqueues through the
official Celery client.

**Why**: the Celery message protocol is Python-centric and non-trivial to
reimplement faithfully in Rust (serialization, headers, acks). Going through the
official client eliminates that whole class of bugs. Accepted cost: a 4th service
to deploy.

**Rejected alternatives**:
- *Producing the Celery format from Rust (rusty-celery or a hand-built message)*:
  fewer services, but a fragile coupling to Celery's internal format.
- *Custom Redis queue + home-grown consumer*: loses Celery's native retries/acks.

**Security**: the FastAPI service is not publicly exposed (internal docker-compose
network) and requires a shared token (`INTERNAL_API_TOKEN`).

### ADR-006 — Returning results: HTTP callback to the Rust API

**Decision**: the worker never touches the database. It posts results to an
internal endpoint of the Rust API: `POST /internal/jobs/{job_id}/results` (same
internal token as ADR-005). If the agent fails: `POST /internal/jobs/{job_id}/failure`.

**Why**: a single application owns the database schema (the Rust backend).
The hexagonal boundary stays clean: on the agent side it is the `ResultSink`
port; on the Rust side it is the `IngestResults` use case.

**Rejected alternative**: direct database writes from the worker — two
applications coupled to the same schema, riskier migrations.

### ADR-007 — Database: PostgreSQL + sqlx

**Decision**: PostgreSQL 16, accessed via `sqlx` (compile-time-checked queries),
migrations with `sqlx migrate`.

**Initial schema**:
- `users(id, email, password_hash, created_at)`
- `research_jobs(id, user_id, keyword, status, error, created_at, completed_at)`
  — `status ∈ {pending, running, completed, failed}`
- `search_results(id, job_id, title, url, snippet, published_at NULLABLE, date_confidence, raw JSONB)`

**Why**: the production standard; JSONB is handy for keeping the agent's raw
response.

**Implementation notes** (2026-07-07):
- Queries are runtime-checked (`sqlx::query`), not macro-checked, so the project
  compiles without a database.
- Migrations run automatically at backend startup (`run_migrations`).
- The backend uses PostgreSQL whenever `DATABASE_URL` is set and falls back to
  the in-memory adapter otherwise (dev without infra; data lost on restart).
- `store_results` uses transactional replace semantics (delete + insert), so a
  worker re-delivery (Celery retry) never duplicates results.
- Integration tests (`backend/tests/postgres_repositories.rs`) are gated on
  `DATABASE_URL`: compose service locally, GitLab service in CI (ADR-012); they
  skip cleanly when unset.
- On the host, the compose PostgreSQL maps to port **5433** to avoid clashing
  with a locally installed PostgreSQL (container-internal port stays 5432).

### ADR-008 — Authentication: JWT

**Decision**: JWT access token (short-lived, ~15 min) + refresh token (rotated,
stored hashed in the database to allow revocation). Passwords hashed with **argon2id**.

- Rust side: `jsonwebtoken` + `argon2` crates; refresh tokens are opaque
  (~244 bits of entropy), stored **SHA-256-hashed** in `refresh_tokens`
  (migration 0002) so a leaked table cannot be replayed.
- **Rotation**: refresh tokens are single use — `/refresh` consumes the
  presented token and issues a new pair; a replayed token gets a 401. Expired
  tokens are purged by the background reaper (ADR-016) and garbage-collected on
  use. *Hardened by ADR-056*: rotation is a **family lineage** with **reuse
  detection** — replaying an already-consumed token revokes the whole family.
- Cookie: `HttpOnly; Secure; SameSite=Strict; Path=/api/auth`, TTL
  `REFRESH_TOKEN_DAYS` (default 30). Scoped to the auth endpoints only.
- Vue side: access token in memory (Pinia store) — never in localStorage; the
  refresh cookie enables **silent session restore** on page reload (router
  guard) and a refresh-and-retry on 401 (`withAuth` in the auth store).
- Endpoints: `POST /api/auth/register`, `/login`, `/refresh`, `/logout`
  (all implemented, 2026-07-07).

Full sequence diagram (sign-up → login → silent refresh → sign-out):
[`docs/diagrams/auth-refresh-flow.puml`](diagrams/auth-refresh-flow.puml)
— render with `plantuml -tpng docs/diagrams/auth-refresh-flow.puml`.

**Rejected alternative**: server-side cookie sessions — simpler, but the JWT
choice prepares for multiple clients / public APIs.

### ADR-009 — Web search: Tavily

**Decision**: Tavily as the default `SearchProvider`.

**Why**: designed for LLM agents, official LangChain integration
(`langchain-tavily`), often returns `published_date` directly, free tier
sufficient for a boilerplate. The `SearchProvider` port allows plugging in
Brave, SerpAPI, or DuckDuckGo without touching the domain.

**Config**: `TAVILY_API_KEY` (environment variable).

**Error handling (added 2026-07-29)**: a Tavily error response (`{"error": …}`
— e.g. an exhausted key quota) **raises**, so the job fails fast with the
provider's message. It is deliberately *not* swallowed as "zero results": doing
so let the agent keep searching against a dead provider, exhausting its step
budget and LLM spend on a run that silently returned nothing. Found via the
cost/decision visibility of ADR-038/029.

### ADR-010 — LLM: Claude (Anthropic) via langchain-anthropic

**Decision**: default model **`claude-opus-4-8`** (Claude Opus 4.8), configurable
via `AGENT_MODEL_ID`. Accessed through `langchain-anthropic` (`ChatAnthropic`).

**Role of the LLM in the agent**: reasoning for the research agent and, above
all, extraction/normalization of publication dates from page content
(see ADR-011). Use adaptive *thinking* (the model's default behavior; do not
configure `budget_tokens`, a parameter removed on this model generation).
Do not pass `temperature`/`top_p` (rejected with a 400 on Opus 4.8).

**Cost reference** (per million tokens, July 2026):

| Model | ID | Input | Output |
|---|---|---|---|
| Claude Opus 4.8 (default) | `claude-opus-4-8` | $5 | $25 |
| Claude Sonnet 5 | `claude-sonnet-5` | $3 ($2 intro) | $15 ($10 intro) |
| Claude Haiku 4.5 | `claude-haiku-4-5` | $1 | $5 |

The `LLM`/`DateExtractor` port is abstract: switching models (or providers) is
configuration, not a rewrite.

**Config**: `ANTHROPIC_API_KEY`, `AGENT_MODEL_ID`.

*Amended by ADR-041 (2026-07-22)*: the same adapters can now run on a local
model instead — `AGENT_LLM_BACKEND=ollama` + `AGENT_LLM_BASE_URL` (then
`AGENT_MODEL_ID` names the local model and `ANTHROPIC_API_KEY` is no longer
required); usage guide in `docs/COMMANDS.md` §“Local LLM”.

### ADR-011 — Publication date extraction: cascade strategy

Publication dates are often missing or wrong. Strategy, in order:

1. **Tavily metadata** (`published_date`) when present → confidence `high`.
2. **Page metadata**: JSON-LD (`datePublished`),
   `<meta property="article:published_time">` tags, OpenGraph → confidence `high`.
3. **LLM extraction** from page content (dates in the text, "published on…"
   mentions) → confidence `medium`, date normalized to ISO 8601.
4. **Failure** → `published_at = NULL`; the source is kept and displayed in an
   "unknown date" section (at the end of the list), never dropped.

Final sort: dates descending, `NULL` last. `date_confidence ∈ {high, medium, unknown}`
is stored and exposed to the frontend.

### ADR-012 — TDD: per-component strategy

General rule: test first; the hexagonal structure makes the domain testable
without I/O.

| Component | Tools | Pyramid |
|---|---|---|
| `backend/` | `cargo test`, hand-written fakes for the ports, PostgreSQL integration tests gated on `DATABASE_URL` (compose service locally, GitLab service in CI), HTTP tests via `tower::ServiceExt` | domain/use-case units ≫ adapter integration ≫ API e2e |
| `agent/` | `pytest`, fakes for the ports (`SearchProvider`, `DateExtractor`, `ResultSink`), `respx`/`responses` for outbound HTTP, Celery in eager mode for task tests | domain units ≫ adapter integration (LLM mocked, never a real call in CI) |
| `frontend/` | `vitest` + `@vue/test-utils` / Testing Library, `msw` to mock the API | components + stores |

**CI**: each component has an independently runnable suite; no test calls a paid
service (Tavily, Anthropic) — those adapters are covered by optional integration
tests behind a flag (`RUN_LIVE_TESTS=1`).

**Implementation note (2026-07-17)**: the live tests exist —
`agent/tests/test_live_providers.py`, one per paid adapter (Tavily search,
Claude enricher/policy/critic). They detect **provider drift** (renamed fields,
a model that stops following the JSON instructions) that the defensive parsing
elsewhere would degrade silently. Skipped without the flag; run them after a
model bump or when extraction quality drops.

### ADR-013 — Development environment: docker-compose

**Decision**: a single `docker-compose.yml` at the repository root covering **all**
bricks (see ADR-014). Two modes via Compose *profiles*:

- `docker compose up` (profile `infra`, default): PostgreSQL + Redis only —
  the Rust backend, the agent, and the frontend run locally (`cargo run`,
  `uvicorn`/`celery`, `npm run dev`) for hot-reload comfort.
- `docker compose --profile full up`: the fully containerized stack, identical
  to what CI builds — useful for validating end-to-end integration.

Configuration comes from `.env` (see `.env.example`). The Rust backend loads
the root `.env` automatically in development (dotenvy); containers get their
configuration from compose/CI environment variables only.

### ADR-014 — Dockerizing every technical brick

**Decision**: every technical brick has its own Docker image, built as a
**multi-stage** build for minimal final images. One source of truth per brick:
the Dockerfile serves dev (profile `full`), CI, and deployment.

| Brick | Dockerfile | Build stage | Final image |
|---|---|---|---|
| Rust backend | `backend/Dockerfile` | `rust:slim` + **cargo-chef** (dependency cache in a separate layer) | `debian:bookworm-slim` (binary only, non-root user) |
| Agent — FastAPI API | `agent/Dockerfile` | `python:3.12-slim` + **uv** (deps installed from `uv.lock`) | same slim base, non-root user |
| Agent — Celery worker | `agent/Dockerfile` (same image) | — | same image, different `command` (`celery -A ... worker`) |
| Vue frontend | `frontend/Dockerfile` | `node:22-alpine` (`npm ci && npm run build`) | `nginx:alpine` serving static files + reverse-proxying `/api` → backend |
| PostgreSQL / Redis | official images (`pgvector/pgvector:pg16` — PostgreSQL 16 + the `vector` extension for RAG, ADR-063; `redis/redis-stack-server` — core Redis for the Celery broker **+** RediSearch for the LangGraph checkpointer, ADR-046; plain `redis:7-alpine` suffices only with `AGENT_ORCHESTRATOR=loop`) | — | — |

**Rules**:
- A single image for the FastAPI API and the Celery worker (same code, same
  deps) — only the `command` differs. Avoids version drift.
- `HEALTHCHECK` on every application service (`/healthz` **liveness** for Axum
  and FastAPI, `celery inspect ping` for the worker); `depends_on: condition:
  service_healthy` in compose. The backend also exposes a `/readyz`
  **readiness** probe (ADR-059) for a load balancer / orchestrator.
- Configuration comes **exclusively from environment variables** (see
  `.env.example`) — no hardcoded values in the images, same images from dev
  to prod.
- Images tagged `$CI_COMMIT_SHORT_SHA` + `latest` on the default branch
  (see ADR-015).
- One `.dockerignore` per brick (excludes `target/`, `node_modules/`, `.venv/`, etc.).
- The nginx `/api` proxy re-resolves the backend hostname per request
  (`resolver 127.0.0.11` + variable `proxy_pass`, added 2026-07-22): a literal
  `proxy_pass` hostname is resolved once at nginx startup, so recreating the
  backend container (new IP) 502'd every API call until the frontend
  container was restarted too.

### ADR-015 — CI/CD: GitLab CI, GitLab registry

**Decision**: GitLab CI pipeline (`.gitlab-ci.yml` at the root), designed for the
monorepo: each brick has its own jobs, triggered only when its files change
(`rules: changes:`), linked as a DAG with `needs:` so nothing waits on global stages.

**Stages**:

```
lint → test → build → publish → deploy
```

| Stage | backend/ (Rust) | agent/ (Python) | frontend/ (Vue) |
|---|---|---|---|
| `lint` | `cargo fmt --check`, `cargo clippy -- -D warnings` | `ruff check`, `ruff format --check`, `mypy` | `eslint`, `vue-tsc --noEmit` |
| `test` | `cargo test` — GitLab services `pgvector/pgvector:pg16` + `redis:7` (`DATABASE_URL`/`REDIS_URL` pointing at the services) | `pytest` (port fakes, Celery in eager mode — no external service required) | `vitest run` |
| `build` | `docker build` of the image via **kaniko** (no privileged Docker-in-Docker) | same | same |
| `publish` | push to the **GitLab Container Registry**: `$CI_REGISTRY_IMAGE/backend:$CI_COMMIT_SHORT_SHA` (+ `latest` on `main`) | same (`/agent`) | same (`/frontend`) |
| `deploy` | **manual** trigger (`when: manual`) on `main`, GitLab environment `production` → **VPS** (see below). |

**Deployment: VPS + docker compose (decided 2026-07-07)**:

- **Target**: a single VPS with Docker + the compose plugin installed. Enough to
  start with; full containerization (ADR-014) makes a later migration to
  Kubernetes or a PaaS painless — same images, only the orchestrator changes.
- **Mechanism**: the `deploy` job connects to the VPS over SSH (protected CI
  variables: `DEPLOY_HOST`, `DEPLOY_USER`, `SSH_PRIVATE_KEY`) and runs
  `docker compose pull && docker compose up -d`.
- **On the VPS**: a `/opt/aiagent/` directory holds the `docker-compose.yml`,
  the repo's `deploy/` directory (prod override + Caddyfile, copied as-is so
  compose paths match) and the production `.env` (secrets entered once by
  hand, never in the repository or in CI). The VPS authenticates against the
  GitLab registry with a read-only **deploy token**.
- **Prod override** (`deploy/docker-compose.prod.yml` — production-only files
  are grouped under `deploy/`, they play no role in local development): adds a
  **Caddy** reverse proxy
  in front (automatic TLS via Let's Encrypt, the only service exposing 80/443)
  in front of the frontend's nginx and the Rust API; pins image tags to
  `$CI_COMMIT_SHORT_SHA` (reproducible deployments, rollback = redeploy the
  previous tag); PostgreSQL and Redis publish no port on the host; named volume
  for PostgreSQL data.
- **Backups**: daily `pg_dump` via cron on the VPS (outside CI scope, documented
  in the upcoming installation runbook).

**Rules and choices**:
- **Per-brick CI cache**: `~/.cargo` + `target/` (keyed on `Cargo.lock`),
  uv cache (keyed on `uv.lock`), `node_modules/` (keyed on `package-lock.json`).
- **Rust integration tests**: rather than testcontainers (which would require
  Docker-in-Docker), CI provides PostgreSQL/Redis as GitLab *services*; the
  tests read `DATABASE_URL`/`REDIS_URL` and therefore behave identically
  locally (testcontainers or compose) and in CI.
- **Image builds with kaniko**: no Docker daemon or privileged runner required;
  layer cache pushed to the registry (`--cache=true`).
- No secret in the repository: `ANTHROPIC_API_KEY`, `TAVILY_API_KEY`, etc. are
  **GitLab CI/CD variables** (masked/protected) and are only used by the
  optional `RUN_LIVE_TESTS` jobs (manual, never in the default pipeline,
  see ADR-012).
- The `publish` job runs only on `main` and tags; merge requests stop after
  `build` (the image is built for validation but not pushed).

### ADR-016 — Job lifecycle robustness (decided 2026-07-07)

**Problem**: without safeguards, a job whose Celery message is lost, whose worker
dies, or whose callback never arrives stays `pending` forever and the frontend
polls indefinitely. Transient failures (network, provider hiccup) failed jobs
that a simple retry would have completed.

**Decision** — three complementary mechanisms:

1. **`running` transition**: the worker notifies
   `POST /internal/jobs/{id}/started` before searching. `start()` only
   transitions `pending → running`; late or duplicate notifications are no-ops.
2. **Backend reaper** (`FailStaleJobs`): a background task (every 60 s) fails
   any job stuck in `pending`/`running` longer than `JOB_TIMEOUT_MINUTES`
   (default 15) with an explicit timeout error.
3. **Celery retries**: `acks_late` + `task_reject_on_worker_lost` (a task is
   re-queued if the worker dies mid-run), and the research task auto-retries
   transient exceptions up to 3 times with exponential backoff + jitter.

**Idempotence makes retries safe** — the invariants that allow re-running the
whole flow at any point:

- `start()` is a no-op unless the job is `pending`;
- result delivery uses replace semantics (ADR-007), never duplicating;
- a **late completion overwrites a timeout failure** (results are valuable —
  the reaper's verdict is not final);
- a **failure never clobbers a completed job** (late duplicate callbacks);
- `report_failure` from the worker is best-effort: if the backend is
  unreachable, the original error still reaches Celery (which retries), and the
  reaper eventually settles the job status.

**Rejected alternative**: heartbeats from the worker during long searches —
more precise staleness detection, but more moving parts than a boilerplate
needs; the timeout covers the realistic failure modes.

### ADR-017 — Abuse protection: rate limiting + per-user quota (decided 2026-07-07)

**Problem**: every search triggers paid Tavily and Anthropic calls, and the auth
endpoints are a brute-force target. Without limits, one user (or one script) can
burn the API budget or hammer login.

**Decision** — two complementary layers:

1. **Per-user daily quota** (business rule, lives in the `LaunchSearch` use
   case): at most `DAILY_SEARCH_QUOTA` searches per user per rolling 24 h
   (default 20), counted via the `count_created_since` port method. Exceeding
   it returns `429` with an explicit message.
2. **Per-IP rate limiting** (HTTP adapter middleware): fixed-window in-memory
   limiter keyed on the client IP — first `X-Forwarded-For` entry (set by
   Caddy/nginx in front, ADR-014/015), falling back to the socket peer address.
   Two knobs: `RATE_LIMIT_AUTH_PER_MINUTE` on `/api/auth/*` (default 10,
   brute-force protection) and `RATE_LIMIT_API_PER_MINUTE` on the rest of
   `/api/*` (default 120). Internal routes (worker traffic) are never limited.
   *Extended by ADR-057*: a third, **per-account** login throttle
   (`LOGIN_MAX_ATTEMPTS_PER_MINUTE`) closes IP-rotating credential-stuffing that
   per-IP limits miss, and a `security_events` audit log records the hits.

**Why in-memory fixed-window**: single-instance deployment (ADR-015) — no shared
store needed, ~80 lines, fully unit-testable. **Trade-offs accepted**: limits
reset on restart, and the IP limiter is per-instance — with N instances the
effective limit becomes N× the configured one (benign degradation; the
per-user quota stays exact at any scale since it counts PostgreSQL rows).
If horizontal scaling makes that matter, prefer rate limiting at the reverse
proxy/load balancer over a Redis-backed limiter in the backend — the latter
only pays off for fine-grained per-user rules (see ROADMAP.md P4).
`X-Forwarded-For` is only trustworthy because the reverse proxy is the sole
public entry point in production.

**Rejected alternatives**: `tower-governor` (an extra dependency and IP-extractor
coupling for behavior we can state in a few dozen lines); quota stored as a
counter table (the existing `research_jobs` table already answers the question
with one `COUNT`).

### ADR-018 — Observability: end-to-end correlation + structured logs (decided 2026-07-07)

**Problem**: a research request crosses four processes (Rust API → FastAPI →
Celery worker → callback to Rust). Without a shared identifier and parseable
logs, diagnosing a production incident means eyeballing four unrelated streams.

**Decision**:

1. **Correlation id**: every HTTP request to the Rust API runs inside a tracing
   span carrying a `request_id` — taken from the incoming `X-Request-Id` header
   (proxy/client) or generated, and echoed on the response. For the
   asynchronous flow, **the `job_id` is the cross-service correlation key**:
   the dispatcher sends it as `X-Request-Id` to the FastAPI micro-API, which
   passes it into the Celery task, and the worker's sink returns it on every
   internal callback. `grep <job_id>` across the four services tells the whole
   story of one search.
2. **Structured logs**: `LOG_FORMAT=json` (set in `deploy/docker-compose.prod.yml`)
   switches every process to one JSON object per line — `tracing-subscriber`'s
   JSON layer (events flattened, span fields included) on the Rust side, a
   stdlib `JsonFormatter` (no extra dependency) on the Python side, with
   `request_id`/`job_id` as first-class fields. Dev keeps human-readable output.

**Why not OpenTelemetry now**: this is 80% of the diagnostic value for a
fraction of the moving parts. The ids and the structure are exactly what an
OTel migration (noted in §5) would need anyway — nothing is thrown away.

### ADR-015 amendment — scheduled security audits (added 2026-07-07)

A sixth stage `audit` runs **only** on scheduled pipelines (plus manual web
triggers): `cargo audit`, `pip-audit` (via `uv export`), `npm audit
--audit-level=high`, and **gitleaks** over the full git history (`GIT_DEPTH: 0`).
Scheduled pipelines skip every other job (`.not-on-schedule` rule), so the
weekly run is audit-only and never blocks merge requests. The weekly schedule
itself is created in the GitLab UI (see SETUP.md §3).

**Advisory exceptions (added 2026-07-18)**: `cargo audit` scans `Cargo.lock`,
which is **feature-agnostic** — it lists optional dependencies the project
never compiles. Exceptions therefore live in `backend/.cargo/audit.toml`
(read by both CIs, which run the audit from `backend/`), and the policy is:
**every ignored advisory carries a written justification and a revisit
condition**, or it is fixed rather than silenced.

Current exception — **RUSTSEC-2023-0071** ("Marvin Attack", timing sidechannel
in `rsa` 0.9.x, no fixed release upstream): `rsa` is a dependency of
`sqlx-mysql` only, and the backend enables the `postgres` driver exclusively
(`sqlx` with `default-features = false`). Both `cargo tree -i rsa --target all`
and `cargo tree -i sqlx-mysql --target all` return nothing: the crate sits in
the lockfile but is never built into the binary, so no RSA code path exists at
runtime. Revisit when `rsa` ships a fix or when sqlx drops the transitive
dependency. Yanked-crate warnings (e.g. `spin` 0.9.8) stay warnings and do not
fail the audit.

### ADR-019 — Open source on GitHub: GitHub Actions becomes the primary CI/CD (decided 2026-07-08, revisits ADR-015)

**Context**: the project goes open source on GitHub. Contributor pull requests
must be validated automatically, and `.gitlab-ci.yml` does not run on GitHub.

**Decision**:

1. **GitHub Actions is the primary CI** (`.github/workflows/`):
   - `ci.yml` — on every PR and push to `main`/tags: lint + test for the three
     bricks (PostgreSQL/Redis as job services, same env vars as GitLab), then
     image builds. PRs build **without pushing** (Dockerfile validation, no
     secrets involved — safe for fork PRs since the test suite calls no paid
     service, ADR-012); pushes to `main`/tags publish to **GHCR**
     (`ghcr.io/christopheduc-me/ai-agent-boilerplate/{backend,agent,frontend}`,
     short-sha + `latest` tags — same scheme as ADR-015).
   - `security.yml` — weekly cron (`0 6 * * 1`) + on-demand: gitleaks (full
     history), cargo/pip/npm audits. No manual schedule to create, unlike GitLab.
2. **The boilerplate repository deploys nothing.** It is a source-code repo:
   forks deploy to their own infrastructure. The VPS deployment story (ADR-015)
   remains fully documented — compose production override, Caddy, provisioning
   checklist in SETUP.md §4, and a **reference deploy job in the GitLab CI
   mirror** — but no GitHub Actions job touches any server.
3. **`.gitlab-ci.yml` is kept as a documented mirror** of the pipeline for
   GitLab-hosted forks (including the reference `deploy:vps` job); it is not
   executed on GitHub. Pipeline changes must update both files.
4. **Branching model: GitHub Flow** — `main` is the only permanent branch
   (protected: PR required, 1 approval, green status checks, squash merge
   only); short-lived branches; contributor flow is fork → branch → PR.
   Contributor-facing rules live in `CONTRIBUTING.md` (English only, TDD, this
   document updated in the same PR); reviews are auto-assigned via `CODEOWNERS`.

**Trade-off accepted**: two pipeline definitions to keep in sync. Acceptable
because the stages are stable and the mirror is clearly marked; the alternative
(dropping GitLab support) would gratuitously narrow the boilerplate's audience.

### ADR-020 — Fail-fast startup validation of environment variables (decided 2026-07-08)

**Problem**: a missing key surfaced only at the first task — the worker started
"healthy" without `TAVILY_API_KEY` and every research job then crashed at
runtime with a deep pydantic stack trace. Configuration gaps must abort startup
with an explicit message, not degrade silently.

**Decision** — per-process required sets, checked at startup (missing **or
empty** — an empty value in `.env` counts as absent):

| Process | Always required | Additionally in `APP_ENV=production` |
|---|---|---|
| agent-worker (`worker_init` signal) | `ANTHROPIC_API_KEY`, `TAVILY_API_KEY` | `INTERNAL_API_TOKEN` not left at a placeholder |
| agent-api (FastAPI lifespan) | — (all vars have dev defaults) | `INTERNAL_API_TOKEN` not left at a placeholder |
| backend (start of `main`) | — (graceful dev fallbacks, ADR-013) | `JWT_SECRET`, `INTERNAL_API_TOKEN`, `DATABASE_URL`, `AGENT_API_URL` — set, non-empty, and not a placeholder (`change-me`, `insecure-dev-secret`) |
| frontend | — (static files, no runtime env) | — |

On failure the process logs one explicit line naming the component and every
missing variable (pointing at `.env.example`) and exits with code 1 — under
compose, the container stops immediately instead of looping on broken tasks.

`APP_ENV=production` is set by `deploy/docker-compose.prod.yml`; development keeps the
graceful fallbacks (in-memory persistence, noop dispatcher, dev secrets with a
warning) so the clone-and-run experience stays intact. The check only fires in
the worker process (Celery `worker_init` signal), so the agent-api container —
which needs no provider key — is unaffected.

### ADR-021 — Fake providers + end-to-end smoke test of the full stack (decided 2026-07-08)

**Problem**: CI validated each brick in isolation but nothing validated the
assembled containerized stack — a regression in a Dockerfile, the nginx proxy
config, a healthcheck, or inter-container networking would pass CI unseen.
And a true e2e run seemed to require paid API keys, which ADR-012 bans from CI.

**Decision**:

1. **Deterministic fake providers** (`agent/src/aiagent/adapters/fake.py`),
   selected with `AGENT_PROVIDERS=fake` (default `live`): no network, no key.
   The fake search returns four fixed hits that exercise the whole date
   cascade (ADR-011) — provider date (high), LLM-extracted date (medium),
   unknown — so one run also asserts sorting and confidence levels. The
   ADR-020 startup gate skips the API-key requirement in fake mode. Selection
   happens in `build_providers()` (tasks layer); the hexagonal core is
   untouched. Fake mode doubles as **keyless local development**.
2. **E2E smoke script** (`scripts/e2e-smoke.sh`): drives the real user journey
   **through the nginx proxy** (register → login → launch → poll until the
   worker completes → assert result order and confidences). Works against any
   base URL (local, CI, staging).
3. **CI job `e2e`**: boots `docker compose --profile full up --build --wait`
   with `AGENT_PROVIDERS=fake` and runs the script — in GitHub Actions
   (native Docker on the runner) and in the GitLab mirror (docker-in-dind,
   base URL `http://docker:8080`). Compose logs are dumped on failure.

**What this covers that unit/integration tests cannot**: image builds, compose
wiring (`depends_on`, healthchecks, `--wait`), nginx `/api` proxying, the
Rust→FastAPI→Celery→callback chain over the real container network, and the
ADR-016 lifecycle (`pending → running → completed`) with a real worker.

### ADR-022 — Platform-agnostic tooling: portable core, thin CI adapters (decided 2026-07-08)

**Problem**: as a boilerplate, the project must not lock its users into one
forge's proprietary tooling (e.g. Dependabot, GitHub-only). Platform-specific
glue is unavoidable (workflows, templates), but logic inside it is not.

**Decision** — the hexagonal principle applied to tooling:

1. **All CI logic lives in portable artifacts**: shell scripts under
   `scripts/` (`e2e-smoke.sh`, `deps-report.sh`), compose files, Dockerfiles,
   and configs of open, multi-platform tools. **Every CI step must be runnable
   locally** with a documented command (docs/COMMANDS.md).
2. **Platform files are thin adapters**: `.github/workflows/` and
   `.gitlab-ci.yml` only install toolchains and call the portable artifacts.
   No business logic in YAML.
3. **Dependency updates without a platform bot**:
   - `scripts/deps-report.sh` — informative outdated report using only the
     native package managers (`cargo update --dry-run`,
     `uv lock --upgrade --dry-run`, `npm outdated`), run weekly by both CIs
     alongside the security audits. Applying upgrades stays a human action
     (creating PRs is inherently a platform API).
   - `renovate.json` — optional automation for forks that want update PRs:
     **Renovate, not Dependabot**, because the same config file works on
     GitHub, GitLab, Bitbucket, Gitea, and self-hosted. The file is inert
     until a Renovate runner is connected.
4. **Neutral integration points**: the image registry is only referenced
   through `CI_REGISTRY_IMAGE`/`IMAGE_TAG` (GHCR, GitLab registry, or any
   other); `CODEOWNERS` sits at the repository root, read by both GitHub and
   GitLab.

**Accepted platform-specific remainders**: issue/PR templates and the workflow
files themselves — adapters by nature, duplicated per forge where useful.

### ADR-023 — Code coverage measurement and reporting (decided 2026-07-09)

**Decision**: the three test jobs measure coverage with each ecosystem's
standard tool — `cargo llvm-cov` (backend), `pytest --cov` (agent),
`vitest --coverage` via v8 (frontend) — and reporting differs per platform:

- **GitHub Actions → Codecov** (per-brick flags, README badge, diff-coverage
  comment on PRs). Codecov passes the ADR-022 filter: it works on GitHub,
  GitLab, Bitbucket, and self-hosted — unlike GitHub-centric alternatives.
  Uploads are tokenless for public repos and **never fail the pipeline**
  (`fail_ci_if_error: false` + `informational: true` in `codecov.yml`):
  coverage is a signal for reviewers, not a gate.
- **GitLab mirror → native `coverage:` regex** (job coverage shown in MRs and
  available as a GitLab badge) — zero external service.

**Thin binaries are excluded, their logic is extracted and tested** (added
2026-07-09): `src/main.rs` and `src/bin/healthcheck.rs` cannot be meaningfully
unit-tested (they bind sockets and block), so their logic moved into library
modules that are — `config::AppConfig` (env parsing, defaults, dev-fallback
warnings, ADR-020 production validation) and `healthcheck::check` (probe
tested against a stub TCP server). The residual shells are excluded from
coverage (`--ignore-filename-regex` + `codecov.yml ignore`); everything they
delegate to is measured. Same philosophy on the agent: `tasks.py` is covered
end to end by calling the Celery task directly with fake providers (ADR-021)
and respx-mocked backend callbacks.

Baseline: backend ≈93 % lines, agent ≈95 %, frontend low by design (only the
domain-critical `ResultList` component is unit-tested — the e2e smoke covers
the views end to end instead, ADR-021).

### ADR-024 — Graceful shutdown of the backend (decided 2026-07-10)

**Problem**: `docker compose stop`, a VPS redeploy, or an orchestrator killing
the container sends SIGTERM; without handling it, in-flight requests are
dropped — including a worker callback in the middle of writing results.

**Decision**: `axum::serve(...).with_graceful_shutdown(...)` on SIGTERM/SIGINT:
the listener stops accepting connections, in-flight requests drain, then the
process exits 0 (compose's default 10 s `stop_grace_period` is the hard cap).
The Celery worker already handles SIGTERM natively (warm shutdown), and the
ADR-016 idempotence covers the truly-interrupted cases — graceful shutdown
makes them rare instead of systematic.

### ADR-025 — Cross-language contract fixtures (decided 2026-07-10)

**Problem**: the internal JSON contracts (backend ↔ FastAPI ↔ worker
callbacks) are serialized by Python and deserialized by Rust (and vice versa),
but each side was tested only against itself — a drift (renamed field, changed
date format, new enum value) would surface in the e2e test at best, in
production at worst.

**Decision**: golden fixtures in `contracts/` (`task-request.json`,
`results-callback.json`, `failure-callback.json`), asserted on **both sides in
the direction of real traffic**: the producer must serialize exactly the
fixture (Python `serialize_result`; Rust `HttpJobDispatcher`, captured by a
stub server), the consumer must accept it (Rust: fixtures POSTed through the
real router; Python: pydantic parse). Tests: `backend/tests/contract.rs`,
`agent/tests/test_contract.py`. A drift now breaks a unit-speed test suite
naming the exact contract.

**Hardened (2026-08-01)**: the agent producer tests were tightened to drive the
**real `HttpResultSink`** (over a mocked transport) and assert the captured
request body equals the shared fixture, for every internal callback with a body
(results, step, usage, question, failure) — previously `failure` and `question`
were only checked against the fixture's *shape* (key set), so an agent-side key
rename would not have broken the contract suite. This also removes a second
source of truth: the real wire output is now pinned to the same fixture the
backend consumes, not to a hand-copied literal.

### ADR-015 amendment — trivy image scanning (added 2026-07-10)

The weekly security audits also scan the three published images for
HIGH/CRITICAL CVEs with **trivy** (`--ignore-unfixed`: only actionable
findings) — the cargo/pip/npm audits cover application dependencies, trivy
covers the **base images** (debian-slim, python-slim, nginx-alpine).

**npm audit resolution (added 2026-07-27)**: unlike `cargo audit`, `npm audit`
has no lockfile-external ignore file, so the policy here is **fix, never
silence** — the gate stays `npm audit --audit-level=high` with no allowlist.
Two high advisories (a `brace-expansion` ReDoS/OOM DoS reachable through the
lint/test toolchain, and a `postcss` build-time path-traversal) were cleared by
upgrading the frontend dev toolchain to current majors: **eslint 9 → 10**
(with `eslint-plugin-vue` 9 → 10), **vitest + @vitest/coverage-v8 3 → 4**, and
**vue-tsc 2 → 3** (Volar 3). No production dependency and no shipped bundle code
was affected — every advisory sat in build/lint/test tooling — and `vite`
stayed on 6 (all three upgrades accept it). The one pin that survives is
`package.json` → `overrides: { "brace-expansion": "5.0.8" }`: the OOM advisory's
vulnerable range is the **continuous** `<=5.0.7`, so *every* published version
except `5.0.8` is affected (the per-major "latests" 1.1.16 / 2.1.2 are still
inside the range). brace-expansion is a single stable `expand()` function across
all majors, so forcing one version on every consumer (minimatch → glob →
js-beautify → `@vue/test-utils`) is safe; `npm ls brace-expansion` shows a
single 5.0.8. `package.json` cannot carry a comment — this note is that pin's
written justification, and its revisit condition is: drop the override once
`@vue/test-utils` (via `js-beautify`) ships a tree that no longer resolves
brace-expansion `<5.0.8`.

### ADR-022 amendment — local pre-commit hooks (added 2026-07-10)

**lefthook** (single multi-platform Go binary) provides opt-in pre-commit
hooks (`lefthook install`): fast format/lint checks per brick plus a gitleaks
staged scan when installed. Deliberately fast — test suites and clippy stay in
CI. Bypass with `git commit --no-verify`.

### ADR-026 — Live job updates over SSE (decided 2026-07-10, revisits ADR-003)

**Decision**: `GET /api/searches/{id}/events` streams the job detail as SSE
`update` events (same JSON shape as the GET endpoint — one shared builder),
one event per change, closing after the terminal status. The frontend
subscribes on the detail view and **falls back to the previous polling**
automatically if the stream fails.

Two deliberate implementation choices:

1. **Per-connection database polling (1 s), not an in-process broadcast** — a
   `tokio::broadcast` bus would be faster but silently wrong with two backend
   instances; the job state lives in PostgreSQL, so polling it keeps the
   stream correct at any scale. Upgrading to Postgres LISTEN/NOTIFY or Redis
   pub/sub is a swap inside `adapters/http/sse.rs` (noted in §5).
2. **Client uses fetch + ReadableStream, not `EventSource`** — the browser's
   `EventSource` API cannot send an `Authorization` header, and the usual
   workaround (access token in the query string) leaks tokens into proxy and
   server logs. A small incremental SSE parser (`createSSEParser`, unit
   tested) keeps the client dependency-free.

### ADR-027 — Timeline: hit enrichment and chronological rendering (decided 2026-07-10)

**Context**: the product's promise is chronological ("what happened about this
keyword, in order"), but results were rendered as a flat list and carried no
information beyond title/snippet/date.

**Decision**:

1. **The `DateExtractor` port becomes `HitEnricher`**: one LLM call per hit
   returns `{published_date, event_type, summary}` as JSON. The ADR-011 date
   cascade is unchanged (provider date wins → high; LLM date → medium; else
   unknown), and the reply parsing is defensive — any malformed piece degrades
   to its neutral value (`other`, no summary) instead of failing the job.
   **Cost note**: every hit now triggers an LLM call (previously only undated
   hits did) — that is the price of badges/summaries; the ADR-017 quota is the
   budget guard, and fake providers keep tests/e2e free.
2. **`event_type`** is a coarse closed enum (`announcement`, `release`,
   `funding`, `legal`, `incident`, `research`, `opinion`, `other`), carried end
   to end (agent → contract → backend column, migration 0003 → frontend
   badge). Unknown values degrade to `other` on the backend (forward
   compatibility); serde defaults keep pre-ADR-027 payloads parseable.
3. **Frontend `ResultTimeline`** replaces the flat list: dated results grouped
   by month, `date_confidence` made visible (solid marker for
   provider-confirmed dates, hollow + "(estimated)" for LLM-extracted ones),
   event badge, LLM summary (falling back to the snippet), and the undated
   section kept apart (ADR-011).

### ADR-028 — Browser-level e2e tests with Playwright (decided 2026-07-11, extends ADR-021)

**Context**: the ADR-021 smoke script exercises the full stack over raw HTTP
but never runs the SPA itself — a broken build, router, store, or the SSE
client (ADR-026) would slip through.

**Decision**: **Playwright** specs in `frontend/e2e/` drive a real Chromium
through nginx against the same fake-provider stack as the smoke script
(`--profile full` + `AGENT_PROVIDERS=fake`; the stack must be up first — no
`webServer` block, the system under test is the containerized one). Two
journeys: register → search → live status → timeline assertions (month groups,
estimated marker, badges, undated section), and login-again → previous
searches (exercises the refresh-cookie flow, ADR-008). Wired into the existing
`e2e` job on both CIs — on GitLab through the official Playwright image run on
the dind daemon (`--network host`, sources `docker cp`'d in; keep the image
tag in sync with `@playwright/test`). Locally: `npm run test:e2e`
(`E2E_BASE_URL` overrides the default `http://localhost:8080`). Playwright
specs are excluded from vitest and from coverage.

### ADR-029 — Opt-in OpenTelemetry traces (decided 2026-07-11, extends ADR-018)

**Context**: the ADR-018 correlation ids make `grep <job_id>` work across the
four processes, but reconstructing latency and causality by hand from logs is
tedious; distributed tracing is the standard answer. A boilerplate, however,
must not force an observability stack on every fork.

**Decision**: OpenTelemetry traces, **strictly opt-in** behind the standard
`OTEL_EXPORTER_OTLP_ENDPOINT` variable — unset or empty (the default), nothing
is installed and both bricks behave exactly as before.

1. **Backend (Rust)**: `telemetry::layer()` adds an OTLP/HTTP export layer to
   the existing `tracing` subscriber; the per-request `http_request` spans
   (ADR-018 middleware) become exported spans for free. The dispatcher injects
   the W3C `traceparent` header on `POST /tasks`, and buffered spans are
   flushed on graceful shutdown (ADR-024).
2. **Agent (Python)**: `configure_telemetry()` installs the OTLP tracer
   provider plus the FastAPI (extracts `traceparent`), Celery (carries the
   context producer → worker through the broker; instrumented per worker
   process) and httpx (propagates it again on the result callbacks)
   instrumentations. One search = one trace across all four hops.
3. **Local trace backend**: `docker compose --profile observability` adds a
   dev-only **Jaeger v2** (OTLP on 4318, UI on 16686); the app services pass
   `OTEL_EXPORTER_OTLP_ENDPOINT` through from the environment. Production
   forks point the variable at their own collector.
4. **Test hygiene (ADR-012)**: unit tests assert the gate (disabled without
   the variable, no header leakage) and the propagation path with an
   exporter-less local provider — no collector, no network.

Metrics and logs stayed out of the original scope: traces are where the
multi-process debugging pain is, and the rest could follow the same opt-in
pattern later — which they since did (log↔trace correlation below, and metrics
in ADR-050).

**Amendment — per-call LLM spans (added 2026-07-29)**: the propagation above
gives one trace per search, but the agent's LLM calls — the expensive,
slow, non-deterministic part — were invisible inside it. The three adapters
(`LlmHitEnricher`/`LlmAgentPolicy`/`LlmResultCritic`) now open **one span per
call** via a shared `llm_span` helper, tagged with the OpenTelemetry **GenAI
semantic conventions** (`gen_ai.operation.name`, `gen_ai.system`,
`gen_ai.request.model`, `gen_ai.usage.input_tokens` / `output_tokens`) plus the
domain outcome that makes a run readable at a glance — `aiagent.agent.action`
(search/ask/finish), `aiagent.critic.dropped` / `has_gap`, the enrichment
`batch_size` (ADR-042 runs the per-hit calls together, so it is one span with
summed usage, not a false per-hit latency). Latency is the span duration; the
`model`/`system` labels are threaded from `Settings` at the wiring point. It
respects the opt-in gate for free: the module tracer is a **no-op proxy** until
`OTEL_EXPORTER_OTLP_ENDPOINT` is set, so the spans cost nothing in the keyless
demo/CI and appear in Jaeger as children of the job trace only when enabled.
Instrumentation lives in the adapter; the domain ports are untouched. Tested
with an in-memory span exporter (`test_llm_spans.py`) — no provider, no paid
call. Together with the cost meter (ADR-038) and the spend cap (ADR-048), this
closes the cost/quality/observability loop for the agent.

**Amendment — log ↔ trace correlation (added 2026-07-29)**: the two pillars now
cross-reference. Each structured log line (ADR-018) also carries the active
`trace_id` / `span_id` when tracing is on — the agent stamps them with a stdlib
`logging.Filter` reading the current OTel span, the backend records the trace id
as a field on the `http_request` span (so `with_current_span` emits it). A no-op
when tracing is off (no active span → the fields are simply absent), so the
keyless output is unchanged. You can now jump from a log line to its Jaeger trace
and back; the `job_id` stays the cross-service key that works even with tracing
off. What to watch and where to find it is written up in
[OBSERVABILITY.md](OBSERVABILITY.md); metrics remain the deferred third pillar
(§5).

### ADR-030 — Two research modes: the workflow and the agentic loop (decided 2026-07-12)

> *Amended by ADR-046 (2026-07-25)*: the agent mode now has **two
> orchestrators** — the LangGraph `StateGraph` (default) and this hand-rolled
> loop (`AGENT_ORCHESTRATOR=loop`). Both drive the same ports; everything below
> about the loop still holds and is asserted for both.

**Context**: the original flow is a *workflow* — a fixed pipeline (one search,
enrich, sort) where the LLM is a component the code calls. A boilerplate named
"AI agent" should also demonstrate an actual **agent**: the LLM deciding the
control flow. Both are legitimate patterns (per the standard workflow-vs-agent
distinction) with different cost/determinism trade-offs, so the boilerplate
now ships both, side by side, on the same plumbing.

**Decision**:

1. **A job has a `mode`** — `workflow` (unchanged pipeline) or `agent` —
   carried end to end: `POST /api/searches` body → jobs table (migration
   0004) → task request contract → Celery task routing. Serde/pydantic
   defaults keep every pre-ADR-030 payload and client working.
2. **The agentic loop** (`run_agent_research`, Python): a new **`AgentPolicy`
   port** (LLM in production, scripted fake under `AGENT_PROVIDERS=fake`)
   sees the goal, the transcript of its own decisions and the collected
   titles, and returns the next action — `search(query, reason)` or
   `finish(reason)`. The loop enforces the mechanics: URL deduplication
   across searches, a **step budget** (`AGENT_MAX_STEPS`, default 5;
   exhaustion forces a reasoned finish) and, alongside it, a **spend cap**
   (`AGENT_MAX_COST_USD`, ADR-048 — money stops the run before the step budget
   does), and the shared enrich/sort/deliver tail (ADR-011/027). The policy
   reply is parsed defensively: anything malformed means finish, never a crash
   or a burned budget.
3. **The live decision journal**: every executed decision is reported through
   a new **`StepReporter` port** → `POST /internal/jobs/{id}/steps` →
   `agent_steps` table (idempotent on `(job_id, seq)`, ADR-016) → the job
   detail payload → the SSE stream (ADR-026, no new streaming code). Journal
   reporting is best-effort by contract: losing a step never fails the job.
   Step `kind` stays an open string end to end so newer agents can add kinds
   without breaking older backends.
4. **Frontend: two demo blocks** on the searches view — "Workflow demo" vs
   "Agent demo" — launching the same form into either mode; agent jobs render
   an `AgentJournal` (query, dedup-aware hit count, the policy's own reason
   verbatim, pulsing "thinking" indicator while live) above the shared
   timeline.
5. **Determinism for tests/e2e (ADR-021)**: `FakeAgentPolicy` scripts
   search → refine (deduplicated to 0 new hits) → reasoned finish, so the
   journal itself demonstrates deduplication and the smoke/Playwright suites
   assert exact step sequences. New contract fixture:
   `agent-step-callback.json` (ADR-025).

Next agentic steps stay in ROADMAP.md: self-critique of results, recurring
searches with memory, human-in-the-loop clarification.

### ADR-031 — Result self-critique before delivery (decided 2026-07-12, extends ADR-030)

**Context**: the ADR-030 loop decides *how to search* but delivers whatever it
collected. A credible agent also judges its own output — "I searched" versus
"I checked that what I found actually answers the goal".

**Decision**: a **`ResultCritic` port** (one LLM call in production, a stable
fake under `AGENT_PROVIDERS=fake`) reviews the collected hits against the goal
once, after the policy finishes and before delivery. The critique returns:

1. an **assessment** (one-two sentences), journaled verbatim as a new
   `critique` step — no backend or contract change needed: the step `kind` is
   an open string end to end (ADR-030) and the frontend renders unknown kinds
   generically;
2. **irrelevant URLs**, dropped from the delivery (the journal reason gets a
   "(dropped N off-topic result(s))" suffix). The prompt tells the critic to
   be conservative — only obvious noise;
3. at most **one gap query**: if set and the `AGENT_MAX_STEPS` search budget
   is not exhausted, the loop runs a single **repair search** (journaled as a
   normal `search` step) and delivers — no re-critique, so the total cost is
   bounded by `max_steps` searches + one critique call.

Parsing is defensive like every LLM reply in this codebase: a malformed
critique degrades to a neutral review (nothing dropped, no gap) and the job
delivers normally. The critic is optional in the use case signature
(`critic=None` keeps the exact ADR-030 behaviour), wired only in agent mode.

### ADR-032 — Human-in-the-loop clarification (decided 2026-07-12, extends ADR-030)

**Context**: an ambiguous goal ("jaguar") makes the agent burn its search
budget on the wrong meaning. A distinctive agent capability is knowing when to
ask instead of guessing.

**Decision**: the policy gains an **`ask` action** — one short question when
the goal is genuinely ambiguous and no clarification is present yet.

1. **Pause**: on `ask`, the loop calls the new `ClarificationRequester` port
   (`POST /internal/jobs/{id}/question`), the job transitions to the new
   **`awaiting_input` status** (migration 0005: status constraint + `question`
   / `answer` columns) and the Celery task ends — a worker never blocks
   waiting for a human. The **reaper ignores `awaiting_input`** (ADR-016
   amendment): the job is paused on the user, not stuck.
2. **Resume**: the user answers via `POST /api/searches/{id}/answer` (owner
   +`awaiting_input` only, else 409). The backend stores the answer, clears
   the journal (replace semantics: the resumed loop starts fresh), flips the
   job back to `pending` and re-dispatches with `clarification` in the task
   request (ADR-025 fixture updated). The task folds the answer into the goal
   — the `AgentPolicy` protocol is unchanged.
3. **One question per job** (cost guard): once a clarification is present (or
   without a clarifier wired), a repeated `ask` degrades to `finish` — no
   question ping-pong. Determinism (ADR-021): the fake policy asks exactly
   when the goal contains "ambiguous" and no clarification yet.
4. **Frontend**: `awaiting_input` renders the question with an answer form in
   the detail view (the SSE stream stays open — not a terminal status); after
   the answer, the dialog stays visible as a recap.

Full sequence diagram: `docs/diagrams/hitl-clarification-flow.puml`.

### ADR-033 — Recurring searches with memory (decided 2026-07-15)

> *Hardened by ADR-053 (2026-07-29)*: the scheduler runs behind a single-leader
> lock, so multiple backend replicas do not launch duplicate recurring jobs.

**Context**: the monitoring use cases (tech watch, real-estate alerts…) need
searches that re-run on their own and can tell what changed — the agent must
live in time, not only answer one-shot requests.

**Decision**:

1. **Saved searches**: a `recurring_searches` table (keyword, mode, interval
   with a 1-minute floor / 7-day ceiling, `last_run_at`) with a minimal CRUD
   (`POST/GET /api/recurring`, `DELETE /api/recurring/{id}`, 20 per user).
   Each run is an ordinary `research_job` linked via `recurring_search_id`
   (kept on deletion, `ON DELETE SET NULL`) — history, results, journal and
   SSE need nothing new.
2. **The scheduler is the backend's background loop** — the existing reaper
   ticker (cadence `SCHEDULER_TICK_SECONDS`, default 60 s) also launches every
   due recurring search. **Rejected: Celery beat** — a fifth process whose
   schedule state would live in the brick that must not own the database
   (ADR-006); the backend ticker already exists and the quota check needs the
   jobs table anyway. Runs count against the owner's daily quota (ADR-017);
   a quota-skipped or failed run still waits for the next interval (never
   retried every tick).
3. **Memory = previously delivered URLs**: the dispatch carries `seen_urls`
   (up to 200 distinct URLs from the search's past runs) in the task request
   (ADR-025 fixture updated). Both modes flag every result with **`is_new`**
   (agent + contract + `search_results.is_new` column, migration 0006 +
   frontend); the agent mode additionally journals a final **`report` step**
   — "N new result(s) since the last run" / "Nothing new since the last run"
   — the agent's own verdict on whether the run was worth it.
4. **Frontend**: a "Recurring searches" block (create/list/delete, last-run
   info); on recurring runs the timeline shows a **new** chip and dims
   already-seen results; one-shot searches are visually unchanged.
5. **Determinism (ADR-021)**: the fake providers return stable URLs, so run 2
   of a recurring fake search reports "Nothing new" — asserted end to end by
   the smoke script (fast tick in the e2e environment).

URL matching uses canonical URLs since ADR-034.

### ADR-034 — Canonical URLs: deduplication and memory matching (decided 2026-07-15)

**Context**: search providers return the same article under cosmetically
different URLs — tracking parameters (`utm_*`, `fbclid`…), fragments, host
casing, parameter order. That double-counts results in one run and, worse,
makes the recurring-search memory (ADR-033) report retagged links as new.

**Decision**: a pure-domain canonicalizer (`domain/urls.py`, stdlib only)
produces the **comparison key**; the displayed URL always stays the original.
Canonical form: scheme/host lowercased, default ports dropped, fragment
dropped, known tracking parameters removed, remaining query sorted, trailing
slash trimmed. Anything unparseable is returned unchanged — a weird URL must
never fail a job, it just deduplicates less well.

Applied at every URL comparison:

1. **workflow mode**: provider hits deduplicated before enrichment (also a
   cost saving: no LLM call for a retagged duplicate);
2. **agentic loop** (ADR-030): cross-search deduplication and the journal's
   `new_hits` counts use canonical keys — including the critique repair
   search (ADR-031);
3. **recurring memory** (ADR-033): `flag_new` canonicalizes both the stored
   URLs and the fresh ones, so a re-tagged link never masquerades as new.

The backend stays unchanged: it stores and forwards raw URLs; canonicalization
is a domain concern of the brick that compares them.

### ADR-035 — Date cascade stage 2: the page's own metadata (decided 2026-07-16, extends ADR-011)

**Context**: for hits without a provider date, the cascade jumped straight to
the LLM — a paid call returning a `medium`-confidence guess, when most
publishers embed the exact date in the page itself.

**Decision**: a **`PageDateFetcher` port** inserted between the provider date
and the LLM: fetch the page (only when the provider gave no date — cost
guard), read **JSON-LD `datePublished`** (object, list and `@graph` shapes)
then **OpenGraph `article:published_time`**. Publisher-declared metadata is
source-authoritative, so it ranks **`high`** — the full cascade is now:
provider (high) → page metadata (high, ADR-035) → LLM (medium) → unknown.

Implementation notes: stdlib parsing (`html.parser` + `json`), bounded fetch
(10 s timeout, download capped at 512 KiB — the metadata lives in `<head>`),
and silent degradation: a dead page, malformed HTML or garbage dates mean "no
date, continue the cascade", never a failed job. The fake stack gains a
`fake-page-datable` hit so the keyless e2e demonstrates every cascade stage.
The LLM enrichment call still runs for every hit (event type + summary,
ADR-027) — stage 2 improves the date, it does not replace the enrichment.

### ADR-036 — Digest webhooks for recurring searches (decided 2026-07-16, extends ADR-033)

> *Hardened by ADR-047 (2026-07-25)*: digests are optionally **HMAC-SHA256
> signed** (`X-Signature-256`) when `DIGEST_SIGNING_SECRET` is set, so a
> consumer can authenticate them; `job_id` remains the at-least-once dedup key.

**Context**: the recurring memory can tell a run found something new, but the
user still has to open the app to notice. Monitoring use cases need a push.

**Decision**: an optional **`webhook_url`** on the recurring search (migration
0007, `http(s)` only). When a recurring run completes **with new results**,
`IngestResults` builds a digest — keyword, run id, `new_count`, and the new
results only (title/url/published_at) — and a **`DigestSender` port**
delivers it. This repository ships the **webhook adapter** (the universal
integration surface: Slack, n8n, Zapier, a fork's endpoint); an e-mail sender
is one more adapter behind the same port.

Rules: strictly **best-effort** (a dead webhook, a deleted recurring search,
or a lookup error are logged and never fail the ingestion), 5 s timeout, no
digest when nothing is new, and the outbound shape is pinned by the
`contracts/digest-webhook.json` fixture (ADR-025 — produced by the backend,
consumed by the user's systems). Note for forks: the URL is user-supplied and
fetched server-side — restrict egress if your deployment is sensitive to SSRF.

### ADR-037 — Opt-in Redis-backed rate limiting (decided 2026-07-16, revisits ADR-017)

**Context**: ADR-017 deliberately kept the per-IP limiter in-memory —
per-instance, benign degradation — and deferred the distributed variant to
"if the backend ever scales horizontally", preferring a reverse-proxy rule.
Forks that do scale out asked for a ready-made in-app option, and the swap
surface was already one file.

**Decision**: a **`RedisWindowLimiter`** behind the existing middleware —
same fixed window, one Redis counter per (scope, client IP, window index) via
`INCR` + `EXPIRE` — activated by **`RATE_LIMIT_REDIS_URL`**; unset keeps the
in-memory limiter (the default stays exactly ADR-017). Two rules worth
noting:

1. **Fail-open**: if Redis is unreachable (tight 1 s budgets), requests pass
   with a warning — the limiter protects LLM spend, it must not turn a Redis
   outage into an API outage.
2. The `auth` and `api` scopes keep independent counters, as before.

The reverse-proxy recommendation of ADR-017 still stands as the zero-code
alternative; this adapter is for forks that want the limit inside the app
(e.g. no shared proxy tier). Integration-tested against the compose/CI Redis
(skipped without `REDIS_URL`), including cross-replica sharing and fail-open.

### ADR-038 — Per-run API spend tracking (decided 2026-07-17)

**Context**: every run spends real money (Claude tokens, Tavily credits) and
the quota (ADR-017) only counts runs, not dollars. Users and forks need to
see what each search actually cost.

**Decision**:

1. **A `UsageMeter`** (pure domain, agent side) is handed to every paid
   adapter: the three Claude adapters record token counts from langchain's
   `usage_metadata` (one `record_llm` per call), the Tavily provider records
   each search. The **fakes record their calls too, with zero tokens** — the
   keyless demo shows honest call counts and a $0 cost.
2. **Pricing is env-driven**, not a hardcoded per-model table (rates rot):
   `LLM_COST_INPUT_PER_MTOK` / `LLM_COST_OUTPUT_PER_MTOK` /
   `SEARCH_COST_PER_CALL`, defaults documented in `.env.example` (Claude
   Opus 4.x and Tavily basic rates at the time of writing). Fake mode prices
   at $0 regardless. Costs are **indicative** — the provider's invoice is the
   source of truth.
3. **One usage callback per task attempt** (`POST /internal/jobs/{id}/usage`,
   fixture `contracts/usage-callback.json`), sent in a `finally` block —
   success, HITL pause and failure all report. The backend **accumulates**
   (`UPDATE … SET x = x + $n`, never replaced by lifecycle updates): retries
   and resumed runs each add their real spend. Best-effort: losing the metric
   never fails the job.
4. **Frontend**: a cost line on the run detail ("$0.0885 — 9 LLM calls
   (8500 in / 1200 out tokens), 2 searches") and, on the searches list, the
   per-run cost plus the total across listed runs.

### ADR-039 — Single-page workbench (decided 2026-07-17, revisits ADR-003's page layout)

**Context**: launching a run navigated to a per-search detail page. For a demo
whose value is *watching* the agent work (live journal, HITL dialog, costs),
the page switch broke the flow — and the user asked for everything on one page.

**Decision**: the searches view becomes a **workbench**: a two-column layout
(launchers + recurring searches + history on the left, the active run on the
right). Launching sets the run **inline** — no navigation; picking a previous
search loads it in the same panel. The follow logic (SSE with polling
fallback, journal, clarification form, cost line, timeline) moves from the
detail view into a reusable **`RunPanel`** component that re-subscribes when
its `id` prop changes and notifies the workbench on terminal status (history
and total cost refresh). The `/searches/:id` route and detail view are
removed — the SPA has two routes left (login, workbench). Deep-linking a
specific run is given up deliberately: this is a demo workbench, not a
document store; a fork wanting shareable URLs can sync the selected run to a
query parameter.

---

### ADR-040 — Ops consoles: Flower + workbench links (decided 2026-07-21)

**Context**: the stack has monitoring UIs (Jaeger for traces, ADR-029) but no
view of the Celery workers themselves — are they up, what ran, what retried,
what failed — and nothing on the workbench points at any of these consoles.

**Decision**: a **Flower** service (`mher/flower`, official Celery monitoring
UI) joins the compose **`observability` profile**, reading the Redis broker
directly — no code change, no new dependency in the agent brick, dev-only like
Jaeger. The workbench gains an **"Ops consoles" card** linking Flower
(`:5555`) and Jaeger (`:16686`) on the app's own hostname, so it also works
when the stack runs on a remote dev box. The links are static: probing each
console to hide dead links was rejected (browsers block the cross-port
`fetch` probes without CORS on the consoles, and the failure mode — a link
that doesn't answer with a stopped profile — is self-explanatory). Production
deployments don't publish these ports (`deploy/docker-compose.prod.yml`
doesn't include the profile); a fork wanting them exposed must put them
behind its own auth proxy — Flower ships none by default.

---

### ADR-041 — Local LLM backend via a chat-model factory (decided 2026-07-22)

**Context**: the live LLM adapters (enricher, policy, critic — ADR-010/030/031)
hardcoded `ChatAnthropic` in their constructors, so running the agent against
a model on the developer's own machine (Ollama) required either code changes
or a whole parallel adapter set.

**Decision**: the LLM *brand* is a construction detail, not a port. The
adapters already typed their logic against langchain's `BaseChatModel` —
prompts, defensive parsing and usage metering are provider-agnostic — so they
now **require the chat model injected** and a single factory
(`adapters/chat_model.py`) builds it from the environment:
`AGENT_LLM_BACKEND=anthropic` (default, hosted, needs `ANTHROPIC_API_KEY`) or
`ollama` (local server at `AGENT_LLM_BASE_URL`, no key; `AGENT_MODEL_ID` then
names the local model). The classes were renamed `Claude*` → `Llm*`
accordingly. Adding a backend (e.g. an OpenAI-compatible local server —
LM Studio, vLLM) is one `elif` in the factory, never a new adapter class:
writing an `OllamaEnricher` would have duplicated every prompt and parser.

Consequences: the worker's fail-fast check (ADR-020) requires
`ANTHROPIC_API_KEY` only on the `anthropic` backend (Tavily stays required in
live mode — search remains remote); the compose worker resolves
`host.docker.internal` (with the Linux `host-gateway` mapping) so a
containerized worker reaches the host's Ollama; with a local backend the
ADR-038 cost rates should be set to 0. Small local models follow the JSON
instructions less reliably — the defensive parsing degrades instead of
failing (ADR-030's guarantee), and an opt-in drift test
(`RUN_OLLAMA_TESTS=1`, mirroring ADR-012) checks a local model against the
same extraction bar as the hosted one. The factory disables Ollama's
*thinking* mode: reasoning models (gemma4, deepseek-r1…) otherwise burn the
whole `num_predict` budget on hidden reasoning and return empty content for
these short strict-JSON tasks — found the hard way, live, on 2026-07-22.

---

### ADR-042 — Batched concurrent enrichment (decided 2026-07-23)

**Context**: both use cases enrich a whole result set, but the `HitEnricher`
port was single-hit and the use cases looped over it — 9 hits meant 9
*sequential* LLM round-trips, the dominant share of a run's wall-clock time
(measured on the ADR-041 full-stack validation).

**Decision**: the port becomes **batch-shaped** — `enrich_many(hits)` returns
one enrichment per hit in order — because that is the shape both callers
actually need; a use case never enriches a single hit. The live adapter
implements it with one `llm.batch(...)` (langchain fans the per-hit calls out
concurrently, replies stay in prompt order), **bounded by `max_concurrency=5`**
so a burst of hits cannot hammer the Anthropic API or overload a local Ollama
(ADR-041). Still one LLM call per hit (ADR-027's cost model is unchanged);
usage is metered on the caller's thread after the batch returns, so the
`UsageMeter` needs no thread-safety. The fakes stay sequential (determinism);
the single-hit `enrich` survives as an adapter convenience for the live drift
tests (ADR-012). Page-date fetches (ADR-035) remain sequential — they only run
for hits without a provider date; parallelizing them is a possible follow-up
if profiles ever show them dominating.

### ADR-043 — Native structured output for the LLM adapters (decided 2026-07-24)

**Context**: the three LLM adapters (enricher, policy, critic) asked the model
to "reply with a single JSON object" in prose and parsed the raw text
defensively. That works but is fragile — precisely where it matters most, on
the small local models the ADR-041 backend enables, which drift from strict
JSON far more than the hosted ones.

**Decision**: each adapter binds a **pydantic reply schema**
(`EnrichmentReply`, `ActionReply`, `CritiqueReply`) through langchain's
`with_structured_output(schema, include_raw=True)` — tool calling on Anthropic
(`function_calling`), grammar-constrained decoding on Ollama (`json_schema`,
its default). The field descriptions carry the instructions the prompts used
to spell out, so the prompts shrink to the task. Conversion functions
(`enrichment_from_reply` etc.) map the validated schema onto the domain types,
still degrading **field by field** (an invented event type → `other`, a prose
date → unknown) so a well-formed but nonsensical reply never voids the whole
result. `include_raw=True` keeps the underlying message so the `UsageMeter`
(ADR-038) still sees token counts.

The legacy text parsers (`parse_enrichment`/`parse_action`/`parse_critique`)
are **kept as a fallback**: when the native path returns no parsed object (a
model that ignored the tool, a schema-validation failure), the raw text runs
through them — the reply degrades, the job never crashes, preserving ADR-030's
"malformed reply is a neutral value, never an exception" guarantee at two
levels now. Conversion is **case-tolerant** on the enum-like fields
(`event_type`, `action`): schema-mode models capitalize freely — found live on
2026-07-24 with gemma4 returning `"Software Release"`, which the strict enum
would have dropped to `other`. The fakes and unit tests exercise both the
structured happy path and the text fallback; the opt-in live tests (ADR-012 /
ADR-041) validate the real tool-use and json_schema paths on each backend.

### ADR-044 — Per-call timeout and retries on the LLM clients (decided 2026-07-24)

**Context**: the chat-model factory (ADR-041) built the clients with no
explicit timeout or retry. A hung call — a cold Ollama loading a large model
into VRAM, a slow network to the Anthropic API — could block a worker for
minutes, and the batched enrichment (ADR-042) makes it worse: one stalled call
holds up the whole batch, hence the whole run. The only safety net was
Celery's task-level retry (ADR-016), which fires on a *failed* task, not on a
merely slow call.

**Decision**: the factory bounds every call, from two env-driven settings with
sane defaults — `AGENT_LLM_TIMEOUT_SECONDS` (60) and `AGENT_LLM_MAX_RETRIES`
(2). On Anthropic they map to the client's native `default_request_timeout`
and `max_retries`. ChatOllama has no direct timeout, so it receives one through
`client_kwargs={"timeout": ...}` (passed to the underlying HTTP client), and
has no built-in retry — a failed local call falls back to the Celery task
retry, which is the right layer for it. The two layers compose: the per-call
timeout caps latency and absorbs transient blips fast; Celery's retry with
backoff still covers a task that fails outright.

### ADR-045 — Model evaluation harness (decided 2026-07-24)

**Context**: the local-LLM backend (ADR-041) raises an immediate question it
gave no way to answer — *which* local model is good enough? The opt-in live
tests (ADR-012/041) are pass/fail drift checks, not a way to compare models
side by side.

**Decision**: a small evaluation harness in `aiagent/evaluation.py` scores a
model on the agent's three LLM capabilities — enrichment, policy, critique —
against a set of **golden cases** with known-good answers, and a CLI prints a
**comparison table** across the models you name
(`python -m aiagent.evaluation ollama:gemma4:latest anthropic:claude-opus-4-8`).
Each capability gets a coarse 0–100% score; the table also shows total latency
and indicative cost (0 for local backends, the ADR-038 env rates for the
hosted one), so the "good enough / fast enough / cheap enough" trade-off is
visible at a glance.

It is explicitly a **directional signal, not a benchmark**: the case set is
tiny and the scoring coarse, enough to separate a model that follows the task
from one that does not, cheaply. It reuses the existing adapters unchanged —
the same `LlmHitEnricher`/`LlmAgentPolicy`/`LlmResultCritic` over a chat model
from the factory (ADR-041) — so it measures exactly what production runs, and a
shared `UsageMeter` (ADR-038) yields the cost column for free. The scoring and
runner are **pure and unit-tested with fakes** (a raised error becomes a
zero-scored result, never stops the sweep); only the CLI touches real, paid
providers, so like the live tests it is invoked by hand, never in CI. Forks
extend the three case lists (`ENRICHMENT_CASES`, `POLICY_CASES`,
`CRITIC_CASES`) with cases from their own domain.

**Amendment — pre-release gate (added 2026-07-27)**: the harness is the
regression net for the part of the system unit tests are blind to. Tests run
against port fakes (ADR-012), so they never touch the real prompts or model; a
prompt edit, a model bump, or a LangChain/LangGraph upgrade can stay CI-green
while the agent's output quality drops. The `--fail-under PCT` flag makes the
live run a **gate**: it prints the table and exits non-zero if any model's
overall score falls under the floor (the pure `failures_below` helper is
unit-tested; the flag validates its 0..1 range before any provider is built).
The deliberate split — the harness's fake-backed scoring/runner already run in
CI via `test_evaluation.py`, but the live gate stays **local, invoked by hand
before a release** — keeps Anthropic keys out of the repo's CI entirely
(GitHub withholds secrets from fork PRs anyway, and a scheduled live run would
spend real budget). Forks that want automation can add a `workflow_dispatch`
job wiring `--fail-under` to their own repo secret; the boilerplate ships only
the local ritual (see COMMANDS.md).

### ADR-046 — LangGraph as the default agent orchestrator (decided 2026-07-25, revisits ADR-030)

**Context**: the agent mode ran on a hand-rolled loop (ADR-030) — pure,
testable, framework-free, and a good teaching artifact. But most projects that
fork this boilerplate build on **LangGraph**, and the loop gave them no
starting point for it. The goal: ship LangGraph as the orchestrator forks start
from, without sacrificing the hexagonal architecture or the loop as a
reference.

**Decision**: a **second orchestrator** for `mode=agent`, selected by
`AGENT_ORCHESTRATOR` (**`langgraph`** default, `loop` keeps ADR-030). It is an
**adapter** (`adapters/orchestration/langgraph_agent.py`): a LangGraph
`StateGraph` whose nodes (`decide`/`search`/`ask`/`finalize`/`critique`) call
the **same domain ports** — so `domain/` and `application/` stay framework-free
and adopting LangGraph is exactly the "swap an adapter, not a rewrite" the
hexagonal split promised. Parity with the loop is asserted test-for-test
(dedup, budget, journal, critique + repair, recurring delta, ask-guard). The
graph topology is diagrammed in `docs/diagrams/langgraph-agent-graph.puml`.

Two capabilities the graph adds:
- **Durable checkpointing** — the graph state is persisted at every super-step
  in **Redis**, keyed by `job_id`. Redis is the worker's own infrastructure
  (the Celery broker), so this respects ADR-006: the worker still never
  touches the database. Postgres — the other obvious store — is therefore
  ruled out here, not just unnecessary. The saver needs **RediSearch**, so the
  compose Redis image becomes `redis/redis-stack-server` (core Redis + the
  module); `redis:7-alpine` works only with `AGENT_ORCHESTRATOR=loop`.
- **Native HITL** (ADR-032) via `interrupt()`: the clarification pause is a
  first-class graph primitive. The worker sees the interrupt, fires the
  `question` callback once (job → `awaiting_input`) and ends; the user's answer
  re-dispatches a task that **resumes the graph from its checkpoint**, so the
  searches done before the pause are not redone — the win over the loop's
  re-dispatch-from-scratch. The pause is detected from the **`invoke()` return
  value** (`__interrupt__`), never from a post-`invoke` `get_state()`: that
  read-back races the Redis checkpoint write and can miss the interrupt,
  silently delivering the empty partial state as a completed job (a
  non-deterministic bug that passed locally and failed in CI, 2026-07-26).
  Reading control signals from the checkpoint store right after writing it is
  the trap; the in-process invoke result is the source of truth.

Design constraints honored: the checkpointed state holds **only JSON-friendly
primitives** (hits/steps as dicts), converted to/from domain types at the node
boundary — the default checkpoint serializer round-trips our frozen dataclasses
only through a *deprecated* pickle fallback, which an exemplary boilerplate must
not depend on. Failure/idempotency semantics match the loop and ADR-016
(deliver replaces, steps upsert on `(job_id, seq)`; a resumed task re-runs the
post-graph enrich/deliver tail, which is idempotent at the backend). The
enrich pass stays batched (ADR-042) and the tail (sort, `flag_new`, deliver)
is shared with the workflow mode.

**Cost**: `langgraph-checkpoint-redis` pulls a heavy transitive chain
(`numpy`, `redisvl`, `ml-dtypes`) for RediSearch/vector features this project
does not use — a deliberate weight trade accepted for a correct, standard,
maintained checkpointer over a hand-rolled one. A fork minimizing image size
can set `AGENT_ORCHESTRATOR=loop` (LangGraph is then unused, though still
installed) or replace the saver with a lean custom `BaseCheckpointSaver`.

### ADR-047 — HMAC signing of digest webhooks (decided 2026-07-25, hardens ADR-036)

**Context**: the digest webhook (ADR-036) POSTs to a user-supplied URL with no
signature. Redelivery is handled — the payload carries `job_id`, so consumers
dedup on it (at-least-once) — but **authenticity** is not: webhook URLs leak
(logs, configs, `Referer`), and anyone who learns the URL can POST a forged
digest the consumer cannot tell from a real one. Dedup stops accidental
doubles; it does nothing against a malicious sender.

**Decision**: an **opt-in HMAC-SHA256 signature**, the same pattern GitHub /
Stripe / Slack use for their webhooks. With `DIGEST_SIGNING_SECRET` set, the
`WebhookDigestSender` signs the exact bytes it sends and adds
`X-Signature-256: sha256=<hex>`; the consumer recomputes
`HMAC-SHA256(secret, raw_body)` and compares in constant time before trusting
the payload. Unset (the default), digests go unsigned — opt-in like the Redis
rate limiter (ADR-037), since the common case is a URL on a trusted internal
tool. It stays a one-adapter concern behind the `DigestSender` port: the domain
and the `job_id` dedup contract are untouched, and the signed bytes are the
canonical body (serialized once, signed and sent), so the consumer verifies
over what it received, never a re-serialization.

Scope kept deliberately small: **body-only** signature (authenticity +
integrity). Replay protection (a signed timestamp the consumer age-checks, à la
Stripe) is a documented extension, not shipped — the digest is idempotent on
`job_id`, so a replay is already a harmless duplicate to a correct consumer.

Consumer verification (Python):

```python
import hashlib, hmac
def valid(secret: str, raw_body: bytes, header: str) -> bool:
    expected = "sha256=" + hmac.new(secret.encode(), raw_body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, header)  # constant-time
```

---

### ADR-048 — Per-job spend cap (cost circuit breaker) (decided 2026-07-29, hardens ADR-030)

**Context**: the agent mode has two bounds — the **step budget**
(`AGENT_MAX_STEPS`, ADR-030) caps the number of decision steps, and
`DAILY_SEARCH_QUOTA` (ADR-017) caps a user's requests. Neither bounds the
**money** a single run spends. The step budget is a coarse proxy: at equal step
counts the cost varies by ~100× with the model and the per-step token size, and
the `UsageMeter` (ADR-038) already computes `cost_usd` — but only *after* the
run, for reporting. A pathological run (a long transcript, a verbose model, a
mis-set expensive model) can burn an unbounded budget while staying under the
step limit. For a fork that wires a real key, a surprise bill is the scariest
failure mode.

**Decision**: a pure-domain `SpendGuard` (`meter` + `pricing` + `cap_usd`) in
`domain/usage.py`, checked by **both orchestrators** (the hand-rolled loop and
the LangGraph `StateGraph`) at the **same seams as the step budget** — before
each `decide`, and to skip the self-critique's extra LLM call / repair search.
When the run's indicative cost crosses `AGENT_MAX_COST_USD`, the agent degrades
to a **clean forced finish** and still delivers what it found — the same
"degrade, never crash" contract as the step budget. It reads the *same live
meter* the adapters feed, so nothing is double-counted, and it stays pure: the
guard is domain, the wiring (meter + env-priced `Pricing` + the setting) is in
the Celery task.

Deliberate choices:

- **Enabled by default at $2.00/job**, well above a normal run (cents). The
  keyless fakes (ADR-021) price at **$0**, so the guard never trips in the
  demo/e2e — it only bites live paid runs, exactly where it should. `0`
  disables it.
- **Model-independent**: a cheap and an expensive model get the same dollar
  ceiling, unlike the step budget.
- **Bounds the tail transitively**: capping the exploration loop caps the number
  of searches, hence the collected hits, hence the batched enrichment cost
  (ADR-042) that runs after the loop.
- **Not deduped across retries** — consistent with ADR-038's "cost is
  intentionally not idempotent": a Celery retry re-spends, and each attempt is
  capped independently.

### ADR-049 — Public API contract fixtures + backward-compat rule (decided 2026-07-29, extends ADR-025)

**Context**: ADR-025 pins the **internal** contract (Rust ↔ Python) with golden
fixtures asserted on both sides. The **public** contract (Rust → Vue) had no
such net: the same shapes (`SearchResult`, `JobUsage`, `AgentStep`, the job
detail) were hand-typed a third time in `frontend/src/api.ts`, with nothing
tying them to what the backend actually serves. A Rust field rename or an added
enum variant compiled, passed every Rust and Python test, and reached the
browser as a silent `undefined` — found by a user, not CI. The boilerplate's
goal is scale-readiness (bricks deployable on separate machines, ADR-006) while
staying a monorepo, so the boundary correctness must be machine-checked.

**Decision**: extend the ADR-025 fixture pattern to the public contract, and add
the missing third side.

1. **Fixtures** `contracts/search-job-detail.json` and `recurring-search.json`
   pin the exact wire shape of `GET /api/searches/{id}` and `/api/recurring`.
2. **Producer (Rust)**: `job_detail_json` / `recurring_search_json` are the
   single serialization path used by the handlers, made `pub` so
   `backend/tests/contract.rs` asserts their output **equals** the fixtures.
3. **Consumer (Vue)**: `api.ts` is rebuilt on **`zod`** schemas as the single
   source of truth — the TS types are derived (`z.infer`, no more hand-written
   duplicates) and every data response is **validated at runtime** (`.parse`),
   so a drift throws a clear `ZodError` client-side instead of an `undefined`.
   `frontend/src/__tests__/contract.spec.ts` validates the same fixtures.

**Backward-compat rule (the scale-ready part)**: because the bricks deploy
independently, a rolling deploy can briefly pair a new frontend with an old
backend (or vice versa). So the contract is **additive by default**: the zod
objects are **non-strict** (unknown fields are stripped, not rejected), an
older frontend keeps working when a newer backend adds a field; and a removal
or rename takes a **deprecation window across two releases**, never a hard flip.
This is the discipline the monorepo's atomic-source guarantee does *not* give
you at the deploy layer.

**Cost**: one dependency (`zod`, ~18 kB gzip in the bundle) — the price of real
runtime validation on the one side that had none.

**Amendment — OpenAPI documentation (added 2026-07-29)**: the public API now
also carries a machine-generated **OpenAPI 3.1 spec** (`utoipa`), served as
interactive **Swagger UI at `/api/docs`** and raw JSON at `/api/openapi.json`
(assets vendored — self-hosted, no CDN). This is **documentation, not a second
source of truth**: the contract stays pinned by the fixtures + zod above; the
spec is derived from the same handlers and DTOs, and a committed
`docs/openapi.json` is drift-checked in CI (`backend/tests/openapi.rs`,
regenerated via `cargo run --example openapi`). One deliberate architectural
note: `utoipa::ToSchema` is derived on a few **domain** wire types
(`SearchResult`, `JobUsage`, `AgentStep`, the enums) alongside the `serde`
derives already there. This does **not** breach the domain-purity rule
(ADR-002/004): `utoipa` core is a representation-level derive with **no I/O** —
same category as `serde` — and the framework pieces (Swagger UI, the `OpenApi`
assembly, the routes) live entirely in the HTTP adapter. Deriving it in the
adapter instead would mean a hand-kept duplicate of every wire shape — exactly
the drift this ADR removes.

A **generated TS/Python client** from that spec (via `openapi-typescript` /
`datamodel-code-generator`) remains deferred (§5): it earns its keep only when
the bricks split into **separate repos**, where a published, versioned schema
becomes the decoupling artifact. Now that the spec exists, that step is cheap.

### ADR-050 — Metrics: the third observability pillar (decided 2026-07-29, extends ADR-029)

**Context**: ADR-029 shipped traces (and its amendments added per-call LLM spans
and log↔trace correlation), explicitly deferring metrics. Traces answer *why
was this run slow/expensive*; they cannot answer *is the fleet healthy, what is
the trend, alert me* — that is the aggregate view, and it is exactly what an
operator of a forked deployment needs.

**Decision**: OpenTelemetry **metrics**, behind the same opt-in gate
(`OTEL_EXPORTER_OTLP_ENDPOINT`) and the same OTLP push as traces.

1. **Agent** emits, via a proxy-meter module (`aiagent/metrics.py`, no-op until
   the provider is installed): `aiagent.llm.call.duration` (histogram),
   `aiagent.llm.tokens` (counter, by operation/type/backend), `aiagent.job.cost`
   and `aiagent.jobs` (counters, by outcome). Recording sits in the adapters and
   the Celery task — the domain stays untouched — reusing the hooks the LLM
   spans already added.
2. **Backend** emits HTTP **RED** metrics from the ADR-018 middleware:
   `http.server.requests` (counter) and `http.server.duration` (histogram), by
   method / **matched route** (`/api/searches/{id}`, low cardinality) / status.
3. **The collector is now the OTLP entry point** (dev-only, observability
   profile): an **OpenTelemetry Collector** receives OTLP and fans it out —
   traces → Jaeger, metrics → a Prometheus scrape endpoint — with **Grafana**
   for dashboards. `OTEL_EXPORTER_OTLP_ENDPOINT` therefore points at
   `otel-collector:4318` instead of Jaeger directly (Jaeger only ingests
   traces); the collector forwards them, so tracing is unchanged. Production
   forks point the variable at their own collector — no code change.

Same discipline as ADR-029: **no-op instruments when telemetry is off** (zero
cost in the keyless demo/CI), instrumentation in the adapters not the domain,
and tested without a live backend — the agent metrics with an in-memory metric
reader (`test_metrics.py`), the backend RED path by the existing middleware
tests recording into a no-op meter, and the whole pipeline once end-to-end
(apps → collector → Prometheus → a provisioned Grafana dashboard). What to watch
and the PromQL for each signal is in [OBSERVABILITY.md](OBSERVABILITY.md); a
starter "AI agent overview" dashboard ships provisioned. The three pillars are
now in place: traces, logs, metrics.

### ADR-051 — Multi-provider search aggregation (decided 2026-07-29, extends ADR-009)

**Context**: a single search engine (ADR-009, Tavily) is a single point of
failure and of recall — different engines index different things, and one
provider's quota/outage (the `Error 432` that motivated the ADR-009 fail-fast
fix) takes the whole run down. A boilerplate for agents should show how to fan a
query across sources.

**Decision**: an `AggregatingSearchProvider` — itself a `SearchProvider`
(the port) — wraps several inner providers, queries them **concurrently**, and
**fuses** their results. Pure adapter-layer composition: the agent loop, the
domain and the ports are untouched (the hexagonal payoff). Selected by
`AGENT_SEARCH_PROVIDERS` (comma-separated: `tavily`, `duckduckgo`); one name
builds the bare adapter, several build the aggregator. A **keyless DuckDuckGo**
adapter ships as the second source, so a fork can run live search with zero
credentials.

Two design choices:

- **Reciprocal Rank Fusion** (`fuse_by_rrf`): a URL's score is `Σ 1/(k+rank)`
  over the engines that returned it (`k=60`), so a result several engines agree
  on outranks a single-engine one, with no weights to tune. Deduplication is by
  canonical URL (ADR-034); the richer hit (one carrying a date) wins a tie.
- **Partial-failure tolerance** — a deliberate departure from ADR-009's
  fail-fast. A single provider must fail the job (silence hides a dead search);
  but with several, a failing or slow engine is logged and the run continues
  with the survivors, each bounded by a per-provider timeout. The aggregator
  raises only when **every** provider fails.

Metering is unchanged (ADR-038): each inner provider records its own credit, so
N engines = N credits per aggregated query. The fusion and the aggregator's
concurrency/tolerance are unit-tested with fake providers (no network); the
concrete DuckDuckGo adapter, like Tavily, is not exercised in CI (ADR-012).

### ADR-052 — LLM fallback chain (decided 2026-07-29, extends ADR-041/044)

**Context**: with search aggregated (ADR-051), the **LLM provider is the biggest
remaining external single point of failure** — if Anthropic is down or the key
is quota'd, *every* agent call fails and the job dies. Per-call retries (ADR-044)
absorb a transient blip; they do nothing for a provider *outage*.

**Decision**: a fallback chain via LangChain's `.with_fallbacks()`, at the same
construction seam as the model factory (ADR-041) — no new port. Each adapter
binds structured output on the primary and each fallback, then chains them:
`structured_with_fallbacks([primary, *fallbacks], schema)`. The primary runs
first; on error the next model is tried, in order. Fallbacks are configured with
`AGENT_MODEL_FALLBACKS` — `backend:model_id` specs (the eval harness grammar,
ADR-045), e.g. `anthropic:claude-haiku-4-5,ollama:qwen3:14b` — so the chain can
cross providers, ending on a **keyless local Ollama** for a last-resort survival
with zero external dependency. Empty by default: a single model means no wrapper,
i.e. the exact previous behavior (backward compatible, so the `fallbacks=[]`
default leaves every existing call site untouched).

The mechanism is unit-tested with fakes whose structured output is a real
`RunnableLambda`, so LangChain's actual fallback path is exercised (a raising
primary, a returning secondary) — no network. Usage metering (ADR-038) reads the
survivor's `usage_metadata`, so cost is attributed to whichever model answered.

### ADR-053 — Single-leader background loop (decided 2026-07-29, hardens ADR-016/033)

**Context**: the backend runs the reaper (ADR-016), the recurring-search
scheduler (ADR-033) and the refresh-token purge (ADR-008) on a shared in-process
ticker (`main.rs`). The API layer is otherwise stateless and horizontally
scalable (JWT sessions, DB-polling SSE ADR-026, the Redis-backed rate limiter
ADR-037) — **except this loop**. With several replicas each would run it every
tick, and the scheduler is **not idempotent**: it would launch *duplicate*
recurring jobs. That is a correctness bug the moment the backend scales past one
instance — exactly the multi-machine target of this boilerplate.

**Decision**: gate the tick behind a `LeaderLock` so exactly one replica runs
the loop. `PostgresLeaderLock` takes a **session advisory lock**
(`pg_try_advisory_lock`) on a fixed key: the first replica to win runs the tick
and releases after, so leadership rotates naturally tick-to-tick; the others
skip. No new infrastructure — Postgres is already there — and no leader election
protocol. The connection that took the lock is **held until release** (a session
advisory lock must be unlocked on the same connection) and returned to the pool
between ticks; if a leader crashes mid-tick its session ends and the lock frees
automatically. The in-memory / single-instance path uses a `NoopLeaderLock` that
always leads. The reaper and purge are idempotent, but gating the whole tick
also spares every replica a redundant DB scan.

Tested: the `NoopLeaderLock` in a unit test, and the Postgres mutual-exclusion
(a second instance is locked out until the first releases) in a `DATABASE_URL`-
gated integration test with a per-run key, so it never contends with a live
instance. This closes the backend's horizontal-scaling story.

### ADR-054 — HTTP security headers (decided 2026-07-29)

**Context**: neither the API, the SPA nor the TLS edge set any security headers
— a standard hardening gap (clickjacking, MIME sniffing, referrer leakage, no
declared content policy).

**Decision**: set them at the layer that owns each response, so there is exactly
one source per header (no duplicates).

- **Backend API** (`security_headers` middleware, outermost so it covers error
  responses too): `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
  `Referrer-Policy: no-referrer`, and a **maximally strict CSP**
  `default-src 'none'; frame-ancestors 'none'` — a browser should never load
  anything from, or frame, a JSON endpoint.
- **Frontend SPA** (nginx, on the HTML `location /` only — the `/api` proxy keeps
  the backend's headers): the same three headers plus the app's real
  **`Content-Security-Policy`** — `default-src 'self'`, `connect-src 'self'`
  (same-origin API + SSE), `script-src 'self'`, `style-src 'self' 'unsafe-inline'`
  (Vue SFC styles), `object-src 'none'`, `frame-ancestors 'none'`. The Vite build
  ships external JS/CSS with no inline scripts, so `script-src 'self'` holds.
- **TLS edge** (Caddy): `Strict-Transport-Security` — HSTS belongs where HTTPS is
  terminated, not on the app behind the proxy over plain HTTP.

The API middleware is unit-tested (headers present on every response, errors
included); the SPA and edge headers were verified live against the running
stack. Tune the SPA CSP if a fork adds an external font/CDN or images from third
parties (`img-src`).

### ADR-055 — Security hardening pass: SSRF, constant-time, weak secrets (decided 2026-07-29)

**Context**: a review of every trust boundary (see the checklist below) found
the auth, cookies (HttpOnly/Secure/SameSite=Strict), CORS (absent = same-origin
only), JWT (HS256 pinned + exp), argon2id, parametrized SQL and the fail-fast
secret gate (ADR-020) all sound — but four gaps worth closing.

1. **SSRF on the digest webhook** (highest — user-controlled URL). `webhook_url`
   was validated for scheme only, and `reqwest` followed redirects with no IP
   filtering: an authenticated user could point a recurring digest at
   `169.254.169.254`, `redis:6379`, the internal API… `WebhookDigestSender` now
   resolves the host and **refuses any non-public address** (loopback, private,
   link-local, CGNAT, ULA…) *before* sending, and the client **disables
   redirects** so a public URL cannot 3xx to an internal one.
2. **SSRF on the page fetcher** (agent, blind). The date-fetch stage pulls result
   URLs — attacker-influenceable. It now applies the same public-host check
   (`is_public_host`, injectable for tests) and **disables redirects**; a blocked
   host degrades silently to "no date", like any other fetch failure.
3. **Non-constant-time internal-token compare**. `check_internal_token` used
   `!=`, a timing side-channel; replaced with a `constant_time_eq` byte compare,
   matching the constant-time care the HMAC path (ADR-047) already takes.
4. **Weak-secret warning**. A short (`< 32` char) `JWT_SECRET` / `INTERNAL_API_TOKEN`
   is a brute-forceable key; the config now warns (not a hard fail, so it never
   breaks an existing deployment).

Each fix is unit-tested (the IP classifier and host guard with literals — no
network; the token compare; the config warning). **Residual risk**: the SSRF
guards resolve then let the client re-resolve, so a determined **DNS-rebinding**
attacker could still slip through — mitigating that fully needs pinning the
connection to the checked IP (a documented follow-up); the guards stop every
realistic case (internal hostnames, literal internal IPs, `localhost`).
*Hardened by ADR-056 (2026-08-01)*: the digest sender now filters addresses in a
custom connect-time DNS resolver, closing that rebinding window.

**Scope note — inter-brick traffic is not affected.** The SSRF guards sit only
on the two surfaces reaching the outside world: the *user-supplied* digest
webhook and *arbitrary result-page* URLs. The trusted internal channels
(backend → agent via `AGENT_API_URL`, agent → backend via `BACKEND_INTERNAL_URL`,
Postgres/Redis) use configured endpoints and never pass through these guards, so
a Docker-network or private-network deployment is unaffected. The one legitimate
case the guard would otherwise block — a fork whose **notification** service
(n8n, a relay) lives on the same private network — is covered by an opt-in,
`DIGEST_ALLOW_PRIVATE_WEBHOOKS=true` (default off): it flips the digest sender's
`allow_private`, allowing internal webhook targets on a trusted network. The
page fetcher keeps no opt-in — result pages are public by nature.

---

### ADR-056 — Security hardening pass 2: token-reuse detection, input caps, DNS-rebinding (decided 2026-08-01)

**Context**: a follow-up to ADR-055 closing three remaining gaps — one it
explicitly deferred (DNS-rebinding) and two on inputs.

1. **Refresh-token reuse detection + family revocation** (ADR-008 hardened).
   Rotation used to *delete* the presented token, so replaying a stolen cookie
   was indistinguishable from an unknown token — the theft went unnoticed while
   both parties kept refreshing. Rotation is now a **lineage**: every token
   carries a `family_id` (one per login, inherited on each rotation), and a
   rotated-away token is **marked `consumed_at`, not deleted**. Replaying a
   consumed token is therefore detectable and treated as a compromise — the
   **whole family is revoked** (`delete_family`), killing the thief's rotated
   token too and forcing re-authentication. Other logins (their own families)
   are untouched. Consumed tokens are kept only until they expire; the existing
   reaper purge (`delete_expired`) cleans them, and logout revokes the family.
   Trade-off: a genuine double-submit of the *same* token (two tabs racing)
   trips the detector and logs that lineage out — the accepted, standard cost of
   reuse detection. Migration `0009` adds `family_id`/`consumed_at` (existing
   rows each get their own family via `gen_random_uuid()`, so they stay usable).
2. **Input length caps** (ADR-017 abuse-protection family). `keyword` (≤ 200),
   the clarification `answer` (≤ 2000) and `webhook_url` (≤ 2048) were bounded
   for emptiness/scheme only — an authenticated user could store multi-KB
   free-text. The **domain** now caps each (measured in `chars`), returning the
   new `JobError::{KeywordTooLong, AnswerTooLong, WebhookUrlTooLong}` → `422`.
   Domain-level so every caller (one-shot, recurring, clarification) inherits it.
3. **DNS-rebinding on the digest webhook** (the ADR-055 follow-up). The
   resolve-then-check pre-flight left a TOCTOU: an attacker's DNS could answer a
   public IP to the check and `127.0.0.1` to the connect. `WebhookDigestSender`
   now installs a custom **`reqwest` DNS resolver** (`PublicOnlyResolver`) that
   filters non-public addresses at connect-time resolution, so the validated and
   the connected addresses are the *same* resolution. `ensure_public_host` stays
   as a fast pre-flight for a clear error; the resolver is the race-free backstop.
   The `DIGEST_ALLOW_PRIVATE_WEBHOOKS` opt-in keeps the default resolver, so
   internal notification targets on a trusted network are unaffected (ADR-055).

Each fix is unit-tested with fakes/literals — no network, no paid service: the
reuse-detection lineage and family-scoped revocation (in-memory + Postgres), the
domain caps, and the resolver (via `localhost`/literal, offline).

---

### ADR-057 — Security audit log + per-account login throttle (decided 2026-08-01)

**Context**: the hardening passes (ADR-055/056) closed attack surfaces but left
abuse **unobservable** and one brute-force vector open. Two additions, both in
the abuse-protection family (ADR-017).

1. **Security audit log.** An append-only `security_events` table (migration
   `0010`) records abuse-relevant moments — `login_failed`, `login_throttled`,
   `refresh_reuse_detected` (ADR-056), `quota_exceeded` (ADR-017) — behind a
   `SecurityAudit` port (in-memory + Postgres). Recording is **best-effort**: it
   never fails the request that triggered it, and every event is *also* emitted
   as a structured log line (ADR-018), so it surfaces in the observability
   pillars (ADR-050) even without a database query. `kind` is free text so a
   fork adds its own kinds without a schema change (like `AgentStep.kind`). The
   `RefreshReuseDetected` event is raised inside `RefreshSession` — the only
   layer that can tell a replay from an unknown token — carrying the `user_id`;
   the edge events (`login_*`, `quota_*`) are raised in the HTTP handlers, where
   the client IP (first `X-Forwarded-For`, trusted proxy — ADR-014/015) is
   known. The background loop purges events older than
   `SECURITY_EVENT_RETENTION_DAYS` (default 90; 0 = keep forever), alongside the
   refresh-token purge. No read endpoint ships — the operator reads it via SQL
   or the logs; a guarded admin route is a documented extension.
2. **Per-account login throttle.** The per-IP limiter (ADR-017/037) does not
   stop credential-stuffing that rotates IPs against **one** account. Login now
   also passes a throttle **keyed by the normalized email**, capped at
   `LOGIN_MAX_ATTEMPTS_PER_MINUTE` (default 10), reusing the exact fixed-window
   `Limiter` (in-memory, or Redis-shared via `RATE_LIMIT_REDIS_URL` when scaled
   out — ADR-037). It is checked **before** the deliberately-costly argon2
   verify, so a throttled account also sheds that CPU load, and it refuses even
   a correct password during the cooldown. Trade-off: an attacker can thus
   briefly lock a victim out (the accepted cost of account throttling); the
   window is short (per-minute) and IP-rotation no longer helps them.

Tested end-to-end with fakes: the audit roundtrip + retention purge (in-memory +
Postgres), the reuse event (`RefreshSession`), and the HTTP throttle + audit
(three logins against a cap-of-2, asserting the `429` and the recorded events).

---

### ADR-058 — Data lifecycle: retention purge + account deletion (decided 2026-08-02)

**Context**: two data-lifecycle gaps for a long-running deployment — search
history grew forever, and there was no way for a user to erase their account
(a GDPR-shaped concern).

1. **Retention purge.** The background loop already purges expired refresh
   tokens (ADR-008) and old security events (ADR-057); it now also deletes
   **finished one-shot searches** (and their results, via the `search_results`
   FK cascade) older than `DATA_RETENTION_DAYS`. Default **0 = disabled** — the
   purge is destructive, so it is opt-in; nothing is deleted unless the operator
   sets it. **Recurring-run history is deliberately spared** (`recurring_search_id
   IS NULL` in the delete): those runs are the dedup memory the scheduler reads
   via `recent_urls_for_recurring` (ADR-033), not disposable log data — purging
   them would make a recurring search re-deliver URLs it already sent. Only
   *finished* jobs are purged, so an in-flight run is never cut off.
2. **Account deletion** (`DELETE /api/account`, authenticated). A `DeleteAccount`
   use case erases the user and all their data — recurring searches, jobs +
   results, refresh tokens, then the user row. The cascade is expressed **through
   the ports** (`delete_all_for_user` on each repository, then `UserRepository::
   delete`), not left to the database's `ON DELETE CASCADE`, so the behavior is
   identical in-memory and on Postgres and is unit-testable with fakes; the FK
   cascade stays as a defense-in-depth safety net. Every step is idempotent, so a
   retry converges. The handler clears the refresh cookie (the session is gone).
   `security_events` for the user are kept but anonymized — their `user_id` FK is
   `ON DELETE SET NULL` (ADR-057), preserving the security trail without the PII
   link.

No new migration — the retention purge and the deletion cascade both use columns
and foreign keys already in place (ADR-006/033/056/057). Tested with fakes and
against Postgres: the purge spares recurring/unfinished/recent jobs, and account
deletion removes every per-user row while leaving other accounts intact. The
frontend exposes account deletion as a topbar action behind a confirm dialog
(auth store `deleteAccount`), with unit coverage and a browser e2e journey
(ADR-028): delete → back on login → the old credentials no longer authenticate.

---

### ADR-059 — Readiness probe (decided 2026-08-02)

**Context**: the containers have a **liveness** check (`/healthz` — "the process
is up", driving the Docker `HEALTHCHECK`, ADR-014), but nothing told a load
balancer or orchestrator whether an instance could actually *serve* — i.e. reach
its database. Liveness and readiness are different signals and must not be
conflated: a transient DB outage should pull an instance from rotation, not trip
the liveness probe and restart-loop the container.

**Decision**: a `GET /readyz` endpoint backed by a `ReadinessProbe` port. The
Postgres adapter runs a cheap `SELECT 1`; the in-memory stack has no external
dependency and is always ready. `/readyz` returns `200` when ready, `503`
otherwise — the standard contract for Kubernetes readiness probes / LB health
checks. Liveness (`/healthz`) is left untouched, so a DB blip drains traffic
(readiness red) without killing the process (liveness green). The probe is a
port like everything else (hexagonal), so it is unit-tested both ways (a fake
not-ready probe → 503) plus a Postgres integration test for the real `SELECT 1`.
Readiness is intentionally DB-only: the agent API being down does not make the
backend unready (dispatch failures are handled gracefully, ADR-005).

---

### ADR-060 — Security-event metrics (decided 2026-08-02, extends ADR-050/057)

**Context**: the security audit log (ADR-057) records abuse events to the DB and
the logs, but there was no **metric** to alert on — an operator could not graph
or page on a spike of failed logins, throttling, refresh-token reuse or quota
hits.

**Decision**: a `MeteredSecurityAudit` **decorator over the `SecurityAudit`
port** emits a `security.events` counter (labelled by `kind`) on every recorded
event, then delegates to the real store. Being a decorator over the port, it
counts *every* call site — the HTTP handlers and the refresh use case — with no
change to those callers, and wraps either backing store (in-memory or Postgres);
`main` composes it around the concrete audit. The counter is a no-op until a
`MeterProvider` is installed (telemetry off, ADR-050), so it is free in the
keyless demo, and `kind` is a closed set so cardinality stays bounded. This is
the same OTel-metrics pattern as the HTTP RED metrics (ADR-050); the decorator
keeps the metering concern out of both the use case and the persistence adapters.

---

### ADR-061 — Per-user notification channels: Slack + Telegram (decided 2026-08-02)

**Context**: digests (ADR-036) could only reach a *per-recurring-search* webhook
URL. Users want to choose, **in their profile**, where their results are
delivered — email, Slack or Telegram — and to use several at once.

**Decision**: a per-user `NotificationChannel` (kind + `target` + optional
`secret`), managed in the profile, delivered to **in addition to** the existing
per-search webhook (kept for backward compatibility, decided with the maintainer).

- **Model** (migration `0011`): `kind` ∈ {`slack`, `telegram`} today (email is
  ADR-062); `target` is the Slack incoming-webhook URL or the Telegram chat id;
  `secret` is the Telegram bot token (stored like the digest signing secret,
  ADR-047 — the user's own integration credential, **never returned by the API**).
  Multiple channels per user; cascades on account deletion (ADR-058).
- **Delivery** — a `ChannelNotifier` port with a `DispatchingChannelNotifier`
  routing by kind: **Slack** POSTs `{"text": …}` to the user URL (so it reuses
  the digest webhook's SSRF guard — filtering DNS resolver, no redirects,
  ADR-055/056, and the `DIGEST_ALLOW_PRIVATE_WEBHOOKS` opt-in); **Telegram**
  POSTs to the fixed Bot API host `sendMessage`. `IngestResults` now sends a
  recurring run's digest to the search webhook (if any) **and** every profile
  channel of the owner — each best-effort and independent, so one dead channel
  never blocks the others or the ingestion.
- **API** (authenticated, ADR-017 rate-limited): `GET /api/account` returns the
  profile (email + channels, no secrets); `POST /api/account/channels`
  `{kind, target, secret?}` adds one (`422` on unknown kind / invalid target);
  `DELETE /api/account/channels/{id}` removes one. A `Profile` use case holds the
  validation; the domain `NotificationChannel::new` enforces the per-kind rules
  (Slack requires `https://`, Telegram requires a token) and the length caps.

Slack and Telegram are plain HTTP POSTs (they reuse `reqwest`, no new heavy
dependency) and are fully tested with local stubs + fakes. Email is deferred to
**ADR-062** because it needs an SMTP stack (`lettre`) that cannot be validated
against a live server here.

---

### ADR-062 — Email notification channel over SMTP (decided 2026-08-02)

**Context**: completing ADR-061 with the third channel — email. The maintainer
chose **SMTP** (universal, works with any provider) over an HTTP email API
(which would have avoided a dependency but tied the boilerplate to one vendor).

**Decision**: an `email` channel kind (migration `0012` widens the `kind` CHECK)
whose `target` is the recipient address (no per-channel secret — SMTP credentials
are server-level). Delivery goes through an **`EmailTransport` port** so the
`EmailNotifier` is testable without a live server:
- `LettreEmailTransport` — the real adapter (`lettre`, SMTP over **STARTTLS +
  rustls**, no OpenSSL), built only when SMTP is configured.
- `DisabledEmailTransport` — returns a clear "not configured" error; also the
  fallback if the SMTP config is invalid at startup.
- A fake transport in tests asserts the email is well-formed (to/subject/body).

**Opt-in + gating**: SMTP is configured via `SMTP_HOST` + `SMTP_FROM` (required
pair) plus `SMTP_PORT` (default 587), `SMTP_USERNAME`, `SMTP_PASSWORD`. When it
is off, `email_enabled` is `false`: the profile advertises it (`GET /api/account`),
the frontend hides the email option, and creating an email channel is refused
(`422`, `ProfileError::EmailNotConfigured`) rather than silently never delivering.

**Validation caveat**: as flagged, the real SMTP send **cannot be tested here**
(no mail server; the "no test calls a live service" rule). The logic is covered
with a fake transport; the actual `lettre` → SMTP path must be **validated once
against a real server by the operator** (like the ADR-012 live-provider tests).

---

### ADR-063 — RAG knowledge base: pgvector + retrieval grounding (decided 2026-08-02)

**Context**: the most broadly-useful capability missing for an AI-agent product
— grounding the agent on the *user's own* documents, not just the web/model.

**Decision**: a per-user knowledge base — upload text/markdown, it is chunked,
embedded and stored as vectors, and the top-k chunks ground the agent's LLM
reasoning (classic RAG). The split respects ADR-006 (the agent never touches the
DB):
- **Backend owns the store** (pgvector in the same Postgres, ADR-007). Migration
  `0013` adds `documents` + `document_chunks(embedding vector(768))` with an
  HNSW cosine index. The compose image becomes `pgvector/pgvector:pg16`.
- **Agent owns embeddings + chunking** (a provider concern, ADR-009/010): an
  `EmbeddingProvider` (deterministic fake for the keyless demo/tests; live via
  `AGENT_EMBED_BACKEND`). **Anthropic has no embeddings API**, so embeddings run
  on a separate provider: **Ollama** `nomic-embed-text` locally (default), or
  **OpenAI** `text-embedding-3-*` in the cloud — requested at `dimensions=768`
  (Matryoshka) so it matches the vector column with no migration. This enables a
  **100%-cloud** setup: Claude (hosted) for reasoning + OpenAI for embeddings.
- **Ingestion**: `POST /api/documents {name, content}` stores it `pending` and
  dispatches an embed task to the agent (`EmbedDispatcher` → agent `/embed`,
  mirroring `JobDispatcher`). The agent chunks + embeds and calls back
  `POST /internal/documents/{id}/chunks`; the backend stores them and marks the
  document `ready` (or `/failure` on error).
- **Retrieval/grounding**: during a job the agent embeds the keyword and calls
  `POST /internal/retrieve {job_id, embedding, k}`; the backend resolves the
  job's owner server-side (the agent never passes a user id, so it can only reach
  the KB of the job it processes — a security boundary) and returns the nearest
  chunks (`embedding <=> $` cosine). The agent injects them into the policy LLM
  context and journals a `knowledge` step.

The retrieval is a hexagonal `DocumentRepository` port (in-memory cosine for
tests, pgvector in prod); the whole backend path is tested with fakes + a real
pgvector integration test. **V1 is text/markdown** — PDF/other parsing is a
documented extension (add a parser before chunking). Changing the embedder's
dimension needs a new migration (the `vector(768)` column is fixed).

---

### ADR-064 — Dependency upgrade wave: sqlx 0.9, reqwest 0.13, Vite 8 (decided 2026-08-03)

**Context**: a periodic sweep of the three bricks against the latest published
versions. Most moves are routine, but four forced a real decision.

**Decision** — upgrade to current majors *except* where the cost is not repaid:

- **sqlx 0.8 → 0.9 (forced)**. `pgvector` 0.4.2 moved its `sqlx` integration to
  0.9, so staying on 0.8 put two incompatible `sqlx` versions in the tree and
  broke `Vector` binding. 0.9 adds the `SqlSafeStr` bound: `sqlx::query` only
  accepts `&'static str`. The five call sites that built SQL by interpolating a
  shared column list now use `concat!` over a **macro** instead of `format!`
  over a `const`, so the strings stay compile-time literals. We deliberately did
  **not** reach for `AssertSqlSafe`: it would silence the new check rather than
  satisfy it, and nothing here needs runtime SQL.
- **sqlx 0.9 raises the MSRV to 1.94**, so `backend/Dockerfile` moves from
  `rust:1.89-slim` to **`rust:1.97-slim-bookworm`**. CI was already unaffected
  (`rust:1-slim` on GitLab, `rust-toolchain@stable` on GitHub). The `-bookworm`
  suffix is not cosmetic: plain `rust:1.97-slim` is Debian 13 (trixie, glibc
  2.41) while the runtime stage is `debian:bookworm-slim` (glibc 2.36), and the
  pairing used to be implicit because `rust:1.89-slim` happened to be bookworm
  too. Building against trixie produced an image that builds and exports
  perfectly, then dies on startup with
  `/app/backend: /lib/aarch64-linux-gnu/libc.so.6: version 'GLIBC_2.38' not found`.
  Only running the container catches this — `docker build` succeeds either way —
  which is why the ADR-028 e2e stack is the acceptance gate for Dockerfile
  changes, not the build alone.
- **reqwest 0.12 → 0.13**, which changes two defaults that matter here. Its
  crypto provider becomes **aws-lc-rs**, while `lettre` brings **ring**: with
  reqwest's plain `rustls` feature, cargo unifies both onto the same `rustls`
  0.23 (verified with `cargo tree -e features -i rustls`, which then reports
  `ring` *and* `aws-lc-rs`). rustls cannot pick a default provider from crate
  features in that state and panics at first use. The backend therefore takes
  the `rustls-no-provider` feature and installs **ring** explicitly, keeping
  provider selection deterministic. That feature makes client construction panic
  unless a provider is installed first, so
  `adapters/tls.rs::ensure_crypto_provider` installs it once per process and is
  called from every client constructor — in the adapters rather than in `main`,
  so unit tests building clients are covered too. Second, 0.13 drops the bundled
  webpki roots for **rustls-platform-verifier**, which reads the system trust
  store: the `debian:bookworm-slim` runtime image therefore now installs
  `ca-certificates`. Without it every outbound HTTPS call (agent dispatch,
  webhooks, Slack, Telegram) would fail to verify at runtime.
- **jsonwebtoken 9 → 11**, with `default-features = false` and the **`aws_lc_rs`**
  backend. Version 11 makes the crypto backend an explicit choice, and the other
  option, `rust_crypto`, pulls in **`rsa`** — the crate ADR-015's advisory review
  keeps out of the tree. `aws-lc-sys` 0.43 was checked to build inside
  `rust:1.97-slim` with **neither cmake nor clang installed**, so the builder
  image needs no extra toolchain. aws-lc-rs stays outside rustls here (it is a
  direct `jsonwebtoken` dependency), so it does not disturb the ring provider
  above.
- **rand_core stays on 0.6**, pinned by `argon2` 0.5.3 → `password-hash` 0.5:
  `SaltString::generate` needs the `RngCore` impl from that same version. Moving
  to `rand_core` 0.10 means `argon2` 0.6, which exists only as `0.6.0-rc.8` —
  0.5.3 *is* the current stable. Password hashing is not where this project
  spends its release-candidate budget; revisit when 0.6.0 ships.

**Agent**: lockfile refreshed and the declared floors realigned on what is
actually tested — several had drifted badly (`langchain-anthropic>=0.3` while
1.5.3 was installed), which let a clean install resolve an API-incompatible
major.

**Frontend**: **vite 6 → 8**, `@vitejs/plugin-vue` 5 → 6, jsdom 25 → 30,
`@types/node` 22 → 26, **pinia 3 → 4** (ESM-only; `@vue/devtools-api` is now a
peer dependency, pulled in explicitly) and **vue-router 4 → 5** (a transition
release — no breaking change without file-based routing). **TypeScript stops at
6.0.3**, and this one is structural rather than a stale pin: TypeScript 7 is the
native (Go) port, whose npm package no longer exports the JavaScript compiler API
(`lib/tsc`, `lib/typescript.js`) — only a `tsc` binary and an `unstable/*` API.
Two independent toolchain packages still need that API, and **both are already at
their newest release**: `vue-tsc` 3.3.9 type-checks SFCs by loading the JS
compiler through `@volar/typescript` and patching `tsc`, and `typescript-eslint`
8.65.0 declares `typescript: >=4.8.4 <6.1.0` and aborts with
`typescript-eslint does not support TS 7.0`. Installing 7.0.2 was tried: it
breaks `npm run lint` as well as `npm run typecheck`, so swapping out `vue-tsc`
alone would not help. No flag or prerelease changes this — vue-tsc's only
concession to 7 is detecting a `typescript` aliased to `@typescript/typescript6`,
which is TypeScript 6 repackaged (`@typescript/old: npm:typescript@^6`), not an
upgrade. **TypeScript 7.1** is the announced release carrying the stable
programmatic API these tools need (~October 2026); Vue, Svelte, Astro and MDX all
wait on it. Tracked in ROADMAP.md; rewriting the SFCs as TSX would unblock 7.x
today but is a frontend architecture change, not a version bump.

---

### ADR-065 — Deep Agents evaluated, deferred (decided 2026-08-03, relates to ADR-046)

**Context**: `deepagents` (LangChain) is the obvious next question for anyone
reading ADR-046, and forks building a different product will ask it before we
do. Recording the evaluation so it is not re-litigated from scratch.

**Where it sits.** Its own docs describe three layers: LangGraph is the runtime
(state, checkpoints, streaming, interrupts), LangChain's `create_agent()` is the
agent abstraction, and Deep Agents is an **opinionated harness** on top
(middleware, backends, subagents, skills, memory). This boilerplate sits on the
*runtime* layer with a hand-written `StateGraph`, so adopting Deep Agents would
replace the graph in `adapters/orchestration/langgraph_agent.py` — not the
checkpointer, the streaming or the `interrupt()`-based HITL, which it also
builds on.

**What it adds**, and nothing else: a planning tool (`write_todos`), **subagents**
with their own context window, and a filesystem backend for offloading
intermediate work out of the context.

**Decision: not now.** The agent mode is four nodes and three typed actions
(`SearchAction` / `AskAction` / `FinishAction`). Planning and subagents solve a
problem this loop does not have, and the harness hands the loop to the model,
which cuts against three guarantees the boilerplate sells:

- the **step budget** (ADR-030), which bounds a runaway loop,
- **defensive parsing** (ADR-043), where a malformed reply degrades to *finish*,
- the **spend cap** (ADR-048), which assumes the call count is predictable.

The `AgentPolicy` port would also lose its purpose — it *is* the "decide the next
action" step — and the SSE journal would stream tool calls instead of the typed
steps (`search`, `critique`, `knowledge`) pinned by the ADR-049 contract
fixtures on all three languages.

**Revisit when the task shape changes.** For a long-deliverable product — a
200-question security questionnaire, a dossier analysis, a tender response — all
three additions become load-bearing at once: the plan is real, one subagent per
section keeps question 180 from inheriting the context of the previous 179, and
context overflow becomes the central problem. That is what *deep* means here.

**Middle path, if only part of it is wanted**: add `Planner` and `SubAgent` as
domain ports, exactly as `AgentPolicy` and `ResultCritic` were, and keep the
budgets, the defensive degradation and the typed steps. More code to own, one
less fast-moving harness to track (ADR-064 is a fresh reminder of that cost).

**Open question, deliberately not answered here**: whether `deepagents` can
honour an externally-imposed spend cap and emit typed journal steps. Answer that
first — it decides whether adoption is a swap or a rewrite of the guardrails.

---

### ADR-066 — Image publication waits for the e2e verdict (decided 2026-08-03, amends ADR-015/019)

**Context**: publishing ran *beside* the end-to-end job instead of behind it. On
GitHub, `build` and `e2e` both declared `needs: [backend, agent, frontend]`, so
they started together and `build` pushed the `sha` and **`latest`** tags to GHCR
without ever seeing the e2e result. The GitLab mirror had the same hole by a
different route: `build` sits in a later stage, but an explicit `needs:`
short-circuits stage ordering, so `backend:build` fired as soon as
`backend:test` went green.

The gap matters because the unit suites run **on the host, not in the image**.
An image can compile, export and pass every test while its binary cannot start —
ADR-064 hit exactly that: a builder base on Debian trixie (glibc 2.41) against a
`bookworm-slim` runtime (2.36), which `docker build` reports as a success and
only `docker run` rejects. In the old ordering that image reaches GHCR and gets
tagged `latest`, the pipeline then turns red, and anything deploying in between
pulls a container that will not boot.

**Decision**: `e2e` joins the `needs` of every image-build job on both CIs —
`needs: [backend, agent, frontend, e2e]` on GitHub, `needs: ["<brick>:test",
"e2e"]` on the three kaniko jobs. Publication is now strictly downstream of the
running stack.

The cost is a few minutes of serialisation on the default branch only.
**Merge/pull requests are unaffected**: they already build without pushing
(`push: false` / `--no-push`), so Dockerfile validation stays as fast as it was
for contributors and forks. The `needs` is deliberately hard rather than
`optional: true` — `e2e` runs on every non-scheduled pipeline, and a missing
gate should fail loudly rather than disappear silently.

Rejected alternative: keep the jobs parallel and smoke-run the image before the
push (`load: true`, check the binary resolves its libraries, then push). It
catches this one failure mode a little sooner, but it restructures a job that
today builds and pushes in a single `docker/build-push-action` step, and it
covers strictly less than the e2e suite already does.

---

### ADR-067 — Prompt guards: a lock in CI, the `ask` branch live (decided 2026-08-03, extends ADR-012/043)

**Context**: the prompts were the one part of the agent no test touched. Every
unit test drives the policy through `FakeAgentPolicy`, and so does the e2e stack
(`AGENT_PROVIDERS=fake`), so editing `POLICY_PROMPT` could weaken or drop an
instruction with the whole suite still green. Defensive parsing (ADR-043) then
makes the failure silent *by design*: an action the parser does not recognise
degrades to *finish*, which is correct behaviour for a confused model and
indistinguishable from a prompt that stopped offering the choice.

The exposed branch was **`ask`** — the entire human-in-the-loop feature
(ADR-032), with its `awaiting_input` status, its reaper exemption and its own
e2e journey. The live suite (ADR-012) covered only the `search` branch, and the
e2e journey drives a scripted fake. Nothing anywhere ran the real prompt through
the `ask` path.

**Decision**, split by what is honestly testable without a model:

- **A prompt lock** (`tests/test_prompt_contract.py`): the sha256 prefix of the
  three prompts is pinned. It has no opinion on wording — it refuses to let a
  prompt change reach `main` unnoticed, and its failure message points at the
  live suite and prints the new hash to paste in. Keyless, so it runs in CI.
- **Structural checks** on what *is* structured: the `ActionReply.action`
  description (what structured output shows the model) must document every
  action label, and every label must round-trip through `action_from_reply`.
  Both derive the labels from the domain action types, so adding an action
  forces both to be updated.
- **A live test for `ask`** (`test_claude_policy_asks_when_the_goal_is_ambiguous`),
  joining the ADR-012 opt-in family behind `RUN_LIVE_TESTS=1`. It is the only
  place the `ask` branch meets a real model.

**Rejected, and worth recording because it looked right**: asserting that
`POLICY_PROMPT` contains each action word. It was implemented, then tested
against a prompt with the ask instruction deleted — and **passed**, because
"ask" survives in "Never ask once a clarification is present", a sentence that
*forbids* asking. A substring check cannot tell an instruction from its
negation. It was dropped rather than kept as false assurance; the lock, which
claims less, is what actually catches the edit.

**Cost for forks**: rewriting the prompts is step 3 of FORKING.md, so a fork
hits the lock immediately. That is the intended first lesson — change the
prompt, run the live suite, update the hash.

---

### ADR-068 — Starter Prometheus alerting rules (decided 2026-08-12, completes ADR-050)

**Context**: the observability stack shipped everything except the part that
wakes someone. `docs/OBSERVABILITY.md` published nine PromQL queries, named
spend rate and the HTTP error ratio as "the two that catch real incidents
first", and instructed forks to "wire Prometheus alerting rules on those before
anything else" — while `deploy/observability/prometheus.yml` carried no
`rule_files` and the repository held no rules at all. It was the only place the
documentation prescribed something the code did not provide, and it was not
marked *(planned)* nor tracked in ROADMAP.md either.

One gap mattered more than the others: the spend cap (ADR-048) bounds **one
job**. A thousand jobs each under their cap still empty the budget overnight,
and nothing watched the aggregate.

**Decision**: six starter rules in `deploy/observability/alerts.yml`, loaded
through `rule_files` and mounted by the `observability` profile. They reuse the
PromQL already published in OBSERVABILITY.md so the two read together:
`AgentSpendRateHigh` (the fleet-level view the per-job cap cannot give),
`HttpErrorRatioHigh`, `AgentJobFailureRatioHigh`, `CollectorScrapeDown`,
`AgentProducesNoJobMetrics`, and `RefreshTokenReuseDetected` — the last with no
threshold, since ADR-057/060 already treats a single replay as a compromise
signal.

Two rules watch the pipeline because **neither implies the other**, a
distinction a code review caught in the first draft. `CollectorScrapeDown`
proves Prometheus can reach the collector's endpoint and nothing more: a
collector that is up but no longer *receiving* OTLP keeps `up` at 1 while every
`aiagent_*` series goes stale. `AgentProducesNoJobMetrics` covers the producer
end — submissions accepted with no outcome returning — and is conditioned on
backend traffic so an idle deployment stays quiet. Both matter because when
metrics stop, every other rule goes silent, and silence is indistinguishable
from healthy; `CollectorScrapeDown` carries a `blackhole` label for an
Alertmanager inhibition rule later.

The failure ratio excludes `paused` from **both sides**. `aiagent_jobs_total`
counts task terminations rather than distinct jobs — a HITL job (ADR-032)
increments `paused` when it pauses and `completed` when the answer resumes it —
so leaving pauses in the denominator lets a burst of clarifications dilute a
real failure spike below the threshold. The first draft filtered only the
numerator; the unit test now pins the corrected behaviour.

**Rules are unit-tested** (`alerts_test.yml`, `promtool test rules`), because an
alert fails in a direction no syntax check catches: a wrong label, a diluted
denominator or an empty-vector comparison makes it silently never fire, which
looks exactly like a healthy system.

Two PromQL properties `AgentProducesNoJobMetrics` depends on are worth naming,
because both were wrong in an earlier draft and neither is visible to
`check rules`. `sum(rate(...))` over a series that does not exist yields an
**empty vector, not 0**, so a bare `== 0` matches nothing — the rule went quiet
precisely when the agent stopped publishing altogether, the failure it exists to
catch; `or vector(0)` fixes it. And a rate window equal to `for` cannot hold a
single submission long enough to reach `firing`, so a lone stuck job never
alerted — the window is now 1h against `for: 15m`, with both sides of the `and`
on the same window so sparse-but-healthy traffic stays silent.

**Thresholds are starting points, not recommendations.** A boilerplate cannot
know a fork's traffic or budget; each one is marked `TUNE` in the file, to be
set from a week of real data. Left untouched they will either cry wolf or stay
silent through a real incident, and the file says exactly that at the top.

**Deliberately not included: Alertmanager.** Rules decide *when* something
fires; routing it to a human is another service in the compose profile and
another decision (which channel, which rotation, which inhibition). Until then
the rules surface at `:9090/alerts` and can back a Grafana alert. Shipping
rules without saying this would imply notifications that never arrive — the
worst possible failure for an alerting system.

**Verified** with `promtool check rules`, `check config` and `test rules`, then
by booting the profile and reading `/api/v1/rules`: all six load, and
`CollectorScrapeDown` was watched transitioning `inactive → pending` against a
stopped collector, so at least one expression is known to match rather than
assumed to. Every unit test was also confirmed to fail against the expression it
guards before being kept — a test that passes both before and after a fix proves
nothing about either.

---

### ADR-069 — Backup restore drill (decided 2026-08-12, completes ADR-015)

**Context**: SETUP.md installs a daily `pg_dump` cron and COMMANDS.md documents
the backup command, but **no restore procedure existed anywhere** and no dump had
ever been restored. That is an unproven backup: the first attempt happens during
an incident, which is the worst moment to discover it does not work.

The risk was not theoretical, because ADR-063 made this more than a plain
PostgreSQL database — `CREATE EXTENSION vector`, a `vector(768)` column and an
HNSW index all have to survive the round trip.

**Decision**: `scripts/backup-restore-drill.sh` restores a dump into a throwaway
container and asserts what this schema actually depends on — migration count,
the `vector` extension, the column type, the HNSW index, a **real cosine query**
against the restored vectors, and row counts matching the source. It exits
non-zero on any mismatch, so it works as a manual drill or a scheduled job. The
restore procedure itself is now in COMMANDS.md §10, where only the backup was.

**What the drill found**, by running it rather than reasoning about it:

- Against a matching `pgvector/pgvector:pg16`, the documented `pg_dump` restores
  **cleanly**: 13 migrations, extension 0.8.6, `vector(768)`, the HNSW index and
  identical row counts. The pgvector risk was real but is handled — the dump
  carries `CREATE EXTENSION IF NOT EXISTS vector` itself.
- The failure mode is elsewhere, and it was **not** the one predicted. Restoring
  into a stock `postgres:16` (16.9) aborted on **line 5** with
  `invalid command \restrict`, leaving **zero tables**. `pg_dump` 16.14 emits a
  `\restrict` directive that an older `psql` does not know. A restore target that
  is merely a few *minor* versions behind the source silently produces an empty
  database, and the error names nothing about versions.

Both directions are covered: the drill was confirmed to exit 0 on a matching
image and 1 on the skewed one, with a message that explains the version rule.

**Also corrected here**: ADR-068 established that `aiagent_jobs_total` counts
task terminations rather than distinct jobs, and fixed the failure-ratio alert
accordingly — but left the two places people actually read that counter. The
Grafana panel and the OBSERVABILITY.md query presented it as per-job, so any
reader over-counted HITL jobs. Both now carry the caveat, with a
`completed|failed` variant for the per-job view.

**Not automated in CI.** The drill needs a database with representative data;
the e2e stack could provide one, but a green drill against a fixture proves less
than a red drill against production data would. It is documented as a manual
exercise to run after any migration touching the knowledge base, and after any
PostgreSQL image bump — which is precisely when the version rule above bites.

---

### ADR-070 — Bound the SSE poll: idle cadence and a stream lifetime (decided 2026-08-12, revisits ADR-026)

**Context**: the SSE stream (ADR-026) polls the database once per second, per
open connection, and each poll costs **three** round-trips —
`find` + `results_for` + `steps_for` in `SearchQueries::get`. It stops when
`job.is_finished()`, which covers only `Completed` and `Failed`.

`awaiting_input` is neither. It is also the one status the reaper deliberately
skips (`WHERE status IN ('pending','running')`), because a user may answer hours
later (ADR-032). So a stream on a job waiting for clarification **never ends
server-side**: it reads the database at 1 Hz for as long as the tab stays open,
against a pool of `max_connections(10)`.

The shape of the problem is the worst kind: the product *invites* it. The UI
shows the question and waits for an answer, so leaving the tab open is the
expected behaviour, not an abuse. The rate limiter (ADR-017) does not help
either — it counts requests per minute, while one request here opens a stream
that then costs 180 queries a minute indefinitely.

**Decision**, two bounds, both small and in `adapters/http/sse.rs`:

- **An idle cadence.** `awaiting_input` polls every 15s instead of 1s. That
  status cannot change except through the user's own POST, and *that* client
  gets its own HTTP response — so the fast cadence buys nothing. The decision
  lives in `poll_interval()`, split out to be testable without a clock.
- **A stream lifetime** of 30 minutes, checked before each read. ADR-026 already
  specifies that the client falls back to polling when the stream fails, so
  closing is safe — and it bounds every case like this one that has not been
  thought of yet.

**Verified**: the lifetime test was confirmed to fail when the bound is removed
(the stream then hangs past the timeout), so it tests the guarantee rather than
the code path.

**Explicitly not measured.** This is a cost read from the code — three queries
per second per idle viewer — not an observed outage. No load test was run, and
on a low-traffic deployment it would never show. It is fixed because the cost is
permanent and silent: it only surfaces as saturation, with no prior signal, and
none of the ADR-068 alerts would name it.

**Left alone**: the per-poll cost itself. Reading a cheap change marker
(status + `updated_at` + counts) before the full detail would cut three queries
to one in the common no-change case. It is a bigger change to `SearchQueries`
and the two bounds above already remove the unbounded part.

---

### ADR-071 — Capacity is configuration, not a constant (decided 2026-08-12, extends ADR-015)

**Context**: this project parameterises everything it asks an operator to
think about — 45 variables covering timeouts, quotas, rate limits, the step
budget, the spend cap, retention. Capacity was the one dimension left out
entirely, and there was no ADR on it:

- the PostgreSQL pool was **hardcoded** at `max_connections(10)`, the only
  tunable in the codebase with no variable, no default documented anywhere and
  no rationale;
- the Celery worker ran **without `--concurrency`**, so Celery forked one worker
  per CPU and the number of parallel LLM jobs silently depended on the host;
- `deploy/docker-compose.prod.yml` set **no resource limits**, while ADR-015's
  recommended topology puts PostgreSQL, Redis, the backend, two agent processes,
  Caddy and the frontend on a single VPS.

Each mattered for a different reason. The pool bounds concurrent *viewers* as
much as concurrent requests, because every open SSE stream borrows a connection
per poll (ADR-070) — a link nothing recorded. The worker concurrency is the
multiplier on the fleet spend rate that `AgentSpendRateHigh` watches (ADR-068):
the per-job cap (ADR-048) bounds one job, concurrency decides how many run at
once, so the bill depended on an undeclared number. And on a single host an
unbounded worker OOM-kills PostgreSQL — the one failure that loses data instead
of merely serving errors.

**Decision**: `DATABASE_MAX_CONNECTIONS` (default 10, floored at 1 — a pool of
zero deadlocks every request instead of failing loudly) and `CELERY_CONCURRENCY`
(default 4; these jobs wait on provider APIs, so the CPU count was the wrong
unit anyway). Memory limits on all six production services, sized for a ~4 GB
VPS and marked as a starting point, with PostgreSQL given the largest
reservation so the kernel evicts anything else first.

**Verified** by resolving both compose files: the limits render as 512M/512M/1G
/128M/1G/256M, and `CELERY_CONCURRENCY` substitutes in both its default and
overridden form. A unit test pins the pool default, an override, and the
non-zero floor.

**Not measured, again.** As with ADR-070 these are numbers read from the
topology, not from a load test — none was run, and no benchmark exists in this
repository. They are explicitly starting points. What the change buys is not the
values but the fact that an operator can now *see* and *change* them, and that
one process can no longer take the host down with it.

*(Superseded in part by ADR-072, which measures the SSE figure.)*

---

### ADR-072 — Capacity baseline against the fake stack (decided 2026-08-16, measures ADR-070/071)

**Context**: ADR-070 and ADR-071 both close with "not measured". They shipped a
poll cadence, a pool size, a worker concurrency and six memory limits, all
reasoned from the code. That is two ADRs' worth of numbers nobody had checked.

The stack makes checking cheap and nobody had exploited it: `AGENT_PROVIDERS=fake`
(ADR-021) is keyless and deterministic, so it can be loaded as hard as you like
with no API bill and no provider variance.

**Decision**: `scripts/load-baseline.py`, run against the fake stack. It parks
jobs in `awaiting_input` — the state ADR-070 found unbounded — holds N SSE
streams open, and samples PostgreSQL's own `xact_commit` counter rather than
trusting the application's accounting.

**What it measured**, and this is the point of the exercise:

| | transactions/s per idle SSE viewer |
|---|---|
| Before ADR-070 (1s cadence) | **3.13** |
| After ADR-070 (15s cadence) | **0.45 – 0.58** |

The "before" row was an accident worth keeping: the first run hit a stale image
built eleven days earlier, so it measured the old code. ADR-070 predicted "three
queries per second per idle viewer" from reading `SearchQueries::get`; the
measurement returned 3.13. The fix is worth roughly a **5×** reduction, and the
residual is shared background work (the scheduler tick) divided across viewers,
not per-viewer cost — so the real SSE saving is larger than the ratio suggests.

**What it refuses to measure, and why that matters more.** An early version
reported a jobs/min figure for `CELERY_CONCURRENCY`. Across identical runs it
returned 1.2k, 9.7k and 12.9k — what varied was the script's own polling
granularity, not the system. Fake providers return instantly, so there is no
latency for concurrency to parallelise: **worker concurrency cannot be
characterised on this stack at all.** The figure was removed rather than
published with caveats. What remains is a burst check — 200 submissions,
~290 accepted/s, all 200 completing — which proves the queue and callback
contract hold under a burst without claiming a throughput number.

**Scope, stated in the script's own output**: these come from a laptop, under
Docker, with fake providers. They are a baseline to re-measure against after a
change, never a capacity promise for a real host. The ADR-071 memory limits and
pool size remain unmeasured — nothing here exercises them.

---

### ADR-073 — Frontend render-error boundary (decided 2026-08-16, extends ADR-018)

**Context**: the frontend was the one tier of this project where a failure left
no trace. The backend records every request with a correlation id (ADR-018),
emits RED metrics (ADR-050) and audits security events; the agent journals its
decisions. `main.ts` had no `errorHandler` and no component used
`onErrorCaptured`, so an exception thrown **during render or a lifecycle hook**
made Vue unmount the whole tree: a blank page, nothing logged, nothing for the
user to act on.

This is a narrower gap than it first looks, and the distinction matters. **API
errors were already handled** — every view carries its own error state, and
`withAuth` retries after a token refresh (ADR-008). What was uncovered is the
other kind: a bug in component code, a shape that slipped past the zod schemas,
a null dereference in a computed.

**Decision**: two layers, because one cannot cover the other.

- `components/ErrorBoundary.vue` wraps the **routed view only**, keyed on
  `route.path`. Scoping it below the topbar is the point: when a view dies the
  header stays usable, so the user can still log out or navigate away rather
  than reload blindly. The fallback offers *Try again* (re-render the subtree —
  no reload, the session survives) and *Back to searches*, and carries
  `role="alert"` so it is announced rather than merely displayed.
- `app.config.errorHandler` in `main.ts` catches what the boundary structurally
  cannot: errors thrown in the topbar, in a store action, in a watcher outside
  that subtree.

**Deliberately no telemetry sink.** Both layers only `console.error`. Choosing
where frontend errors are shipped — Sentry, an OTLP collector, a homegrown
endpoint — is a fork's decision, and picking one here would impose a dependency
and a data-protection question on every fork. The handler bodies are the single
place to change, and the whole app is covered. So the honest gain today is
**interface resilience, not observability**: the user gets something actionable
instead of a white screen; the log stays in their browser console.

**Verified**: five tests, and the suite was confirmed to fail (3 of 5) when the
boundary's capture is commented out, so it tests the behaviour rather than the
code path.

---

## 4. API contracts (summary)

### Public (Vue → Rust)

| Method | Route | Description |
|---|---|---|
| POST | `/api/auth/register` | Account creation |
| POST | `/api/auth/login` | Login → access token (body) + refresh cookie |
| POST | `/api/auth/refresh` | Rotates the refresh cookie → new access token |
| POST | `/api/auth/logout` | Revokes the refresh token, clears the cookie |
| GET | `/api/account` | Profile: email + notification channels (ADR-061) |
| DELETE | `/api/account` | Deletes the account and all its data — jobs, results, recurring, tokens (ADR-058) |
| POST | `/api/account/channels` | Adds a notification channel `{kind, target, secret?}` — `slack`/`telegram`/`email` (ADR-061/062) |
| DELETE | `/api/account/channels/{id}` | Removes a notification channel (ADR-061) |
| GET | `/api/documents` | Lists the user's knowledge-base documents + status (ADR-063) |
| POST | `/api/documents` | Uploads a document `{name, content}` to embed for RAG grounding (ADR-063) |
| DELETE | `/api/documents/{id}` | Deletes a document and its chunks (ADR-063) |
| POST | `/api/searches` | Launches a search `{keyword, mode?}` → `{job_id}` (`mode`: `workflow` default, or `agent` — ADR-030) |
| GET | `/api/searches` | List of the user's searches |
| GET | `/api/searches/{id}` | Status + results sorted by date + agent journal `steps` (ADR-030) + `question`/`answer` (ADR-032) |
| POST | `/api/searches/{id}/answer` | Answers the agent's clarification `{answer}` → job resumes (ADR-032; 409 unless `awaiting_input`) |
| GET | `/api/searches/{id}/events` | SSE stream of the same payload, one `update` event per change, closes on terminal status (ADR-026) |
| POST | `/api/recurring` | Saves a recurring search `{keyword, mode?, interval_minutes, webhook_url?}` (ADR-033/036) |
| GET | `/api/recurring` | Lists the user's recurring searches |
| DELETE | `/api/recurring/{id}` | Deletes a recurring search (run history is kept) |

All `/api/*` routes can answer `429` (per-IP rate limit; `POST /api/searches`
also enforces the per-user daily quota — ADR-017; `POST /api/auth/login` also
enforces a per-account throttle — ADR-057).

### Internal (Rust → FastAPI, shared token)

| Method | Route | Description |
|---|---|---|
| POST | `/tasks` | `{job_id, keyword, mode, clarification?, seen_urls}` → Celery enqueue |
| POST | `/embed` | `{document_id, name, content}` → Celery enqueue the embedding of a KB document (ADR-063) |

### Outbound (Rust → the user's systems)

| Method | Route | Description |
|---|---|---|
| POST | *the saved `webhook_url`* | Digest of a recurring run with news `{recurring_search_id, job_id, keyword, new_count, new_results[]}` (ADR-036, fixture `contracts/digest-webhook.json`). Dedup on `job_id` (at-least-once). Signed `X-Signature-256: sha256=<HMAC-SHA256 of the body>` when `DIGEST_SIGNING_SECRET` is set (ADR-047) |

### Internal (worker → Rust, shared token)

| Method | Route | Description |
|---|---|---|
| POST | `/internal/jobs/{id}/started` | Worker picked the job up → status `running` (ADR-016) |
| POST | `/internal/jobs/{id}/results` | Delivers results `[{title, url, snippet, published_at, date_confidence, event_type, summary, is_new, raw}]` |
| POST | `/internal/jobs/{id}/steps` | Records one agent-loop decision `{seq, kind, detail, reason, new_hits}` (ADR-030, idempotent on seq) |
| POST | `/internal/jobs/{id}/question` | The agent asks the user `{question}` → status `awaiting_input` (ADR-032) |
| POST | `/internal/jobs/{id}/usage` | One task attempt's spend `{llm_calls, llm_input_tokens, llm_output_tokens, search_calls, cost_usd}` — accumulated (ADR-038) |
| POST | `/internal/jobs/{id}/failure` | Reports failure `{error}` |
| POST | `/internal/documents/{id}/chunks` | Embedded chunks of a KB document `{chunks:[{seq, content, embedding}]}` → status `ready` (ADR-063) |
| POST | `/internal/documents/{id}/failure` | Embedding failed `{error}` → status `failed` (ADR-063) |
| POST | `/internal/retrieve` | Nearest KB chunks for grounding `{job_id, embedding, k}` → `{chunks:[{content, document_name}]}` (ADR-063) |

---

## 5. Possible evolutions (out of the boilerplate's scope)

- Migrating the VPS deployment (ADR-015) to Kubernetes or a PaaS if scaling
  needs arise — the images being identical, only the orchestrator changes.
- Upgrade the SSE change-detection (ADR-026) from per-connection DB polling to
  Postgres LISTEN/NOTIFY or Redis pub/sub if connection counts grow.
- ~~Multiple search providers with aggregation/deduplication.~~ Done — ADR-051
  (`AggregatingSearchProvider`, RRF fusion, Tavily + keyless DuckDuckGo).
- An e-mail `DigestSender` adapter (SMTP/SES) behind the ADR-036 port, for
  forks that prefer inboxes over webhooks.
- **Generated API client from the OpenAPI schema.** The `openapi.json` already
  exists (ADR-049 amendment: `utoipa`, served at `/api/docs`) but is used only
  as **documentation** — the contract is pinned by fixtures + zod, proportionate
  while the boilerplate is a **monorepo** (one CI validates every side
  atomically). If a fork splits the bricks into **separate repos**, promote the
  schema to the source of truth: publish a versioned `openapi.json` as the
  release artifact and **generate** the TS client (and Python models) from it
  (`openapi-typescript` / `datamodel-code-generator`) — the published schema is
  then the inter-repo decoupling mechanism the co-located fixtures can no longer
  be. Independent *deployment* is already supported (ADR-006); this is only
  needed for independent *repositories/teams*.
- **Anthropic prompt caching — measured and deferred (2026-07-26).** The stable
  prefix of each live LLM call (the structured-output tool schema + the prompt
  instructions) is 733–928 tokens on `claude-opus-4-8` — below its 1024-token
  cache minimum, so a `cache_control` breakpoint would silently no-op
  (`cache_creation_input_tokens: 0`). Measured with `count_tokens`; the tool
  schema (ADR-043) is the bulk (~630–763 tokens), the instructions ~100–180.
  The prefix clears the **512-token** minimum on Opus 5, and the batched
  enrichment (ADR-042) reuses one prefix across a run — so revisit this if the
  default model moves to Opus 5 (or a lower cache minimum ships).
