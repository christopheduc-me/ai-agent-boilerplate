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
> tracked in `TODO.md`.

Last updated: 2026-07-07

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

**Refresh**: polling every 2–3 s while the job is `pending`/`running`.
SSE/WebSocket is a possible later evolution (noted as such, not in the boilerplate).

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
  presented token and issues a new pair; a replayed token gets a 401 (the
  session was either legitimately rotated or stolen; both warrant
  re-authentication). Expired tokens are purged by the background reaper
  (ADR-016) and garbage-collected on use.
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
| PostgreSQL / Redis | official images (`postgres:16-alpine`, `redis:7-alpine`) | — | — |

**Rules**:
- A single image for the FastAPI API and the Celery worker (same code, same
  deps) — only the `command` differs. Avoids version drift.
- `HEALTHCHECK` on every application service (`/healthz` for Axum and FastAPI,
  `celery inspect ping` for the worker); `depends_on: condition: service_healthy`
  in compose.
- Configuration comes **exclusively from environment variables** (see
  `.env.example`) — no hardcoded values in the images, same images from dev
  to prod.
- Images tagged `$CI_COMMIT_SHORT_SHA` + `latest` on the default branch
  (see ADR-015).
- One `.dockerignore` per brick (excludes `target/`, `node_modules/`, `.venv/`, etc.).

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
| `test` | `cargo test` — GitLab services `postgres:16` + `redis:7` (`DATABASE_URL`/`REDIS_URL` pointing at the services) | `pytest` (port fakes, Celery in eager mode — no external service required) | `vitest run` |
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
- **On the VPS**: a `/opt/aiagent/` directory holds the `docker-compose.yml`
  (+ prod override) and the production `.env` (secrets entered once by hand,
  never in the repository or in CI). The VPS authenticates against the GitLab
  registry with a read-only **deploy token**.
- **Prod override** (`docker-compose.prod.yml`): adds a **Caddy** reverse proxy
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

**Why in-memory fixed-window**: single-instance deployment (ADR-015) — no shared
store needed, ~80 lines, fully unit-testable. **Trade-offs accepted**: limits
reset on restart, and horizontal scaling would need a Redis-backed limiter
(noted in §5). `X-Forwarded-For` is only trustworthy because the reverse proxy
is the sole public entry point in production.

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
2. **Structured logs**: `LOG_FORMAT=json` (set in `docker-compose.prod.yml`)
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
itself is created in the GitLab UI (see TODO.md §3).

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
   checklist in TODO.md §4, and a **reference deploy job in the GitLab CI
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

`APP_ENV=production` is set by `docker-compose.prod.yml`; development keeps the
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

### ADR-015 amendment — trivy image scanning (added 2026-07-10)

The weekly security audits also scan the three published images for
HIGH/CRITICAL CVEs with **trivy** (`--ignore-unfixed`: only actionable
findings) — the cargo/pip/npm audits cover application dependencies, trivy
covers the **base images** (debian-slim, python-slim, nginx-alpine).

### ADR-022 amendment — local pre-commit hooks (added 2026-07-10)

**lefthook** (single multi-platform Go binary) provides opt-in pre-commit
hooks (`lefthook install`): fast format/lint checks per brick plus a gitleaks
staged scan when installed. Deliberately fast — test suites and clippy stay in
CI. Bypass with `git commit --no-verify`.

---

## 4. API contracts (summary)

### Public (Vue → Rust)

| Method | Route | Description |
|---|---|---|
| POST | `/api/auth/register` | Account creation |
| POST | `/api/auth/login` | Login → access token (body) + refresh cookie |
| POST | `/api/auth/refresh` | Rotates the refresh cookie → new access token |
| POST | `/api/auth/logout` | Revokes the refresh token, clears the cookie |
| POST | `/api/searches` | Launches a search `{keyword}` → `{job_id}` |
| GET | `/api/searches` | List of the user's searches |
| GET | `/api/searches/{id}` | Status + results sorted by date |

All `/api/*` routes can answer `429` (per-IP rate limit; `POST /api/searches`
also enforces the per-user daily quota — ADR-017).

### Internal (Rust → FastAPI, shared token)

| Method | Route | Description |
|---|---|---|
| POST | `/tasks` | `{job_id, keyword}` → Celery enqueue |

### Internal (worker → Rust, shared token)

| Method | Route | Description |
|---|---|---|
| POST | `/internal/jobs/{id}/started` | Worker picked the job up → status `running` (ADR-016) |
| POST | `/internal/jobs/{id}/results` | Delivers results `[{title, url, snippet, published_at, date_confidence, raw}]` |
| POST | `/internal/jobs/{id}/failure` | Reports failure `{error}` |

---

## 5. Possible evolutions (out of the boilerplate's scope)

- Migrating the VPS deployment (ADR-015) to Kubernetes or a PaaS if scaling
  needs arise — the images being identical, only the orchestrator changes.
- SSE or WebSocket for real-time job tracking (replaces polling).
- Multiple search providers with aggregation/deduplication.
- Recurring keyword monitoring (Celery beat).
- Per-user quotas / rate limiting.
- Observability: OpenTelemetry traces across the three components.
