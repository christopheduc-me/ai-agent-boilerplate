# AI Agent Boilerplate

[![CI](https://github.com/christopheduc-me/ai-agent-boilerplate/actions/workflows/ci.yml/badge.svg)](https://github.com/christopheduc-me/ai-agent-boilerplate/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**A production-shaped, fully documented boilerplate for building AI-agent web
applications** — Rust API, Python agent workers, Vue frontend, wired together
with the plumbing real deployments need.

The demo product is deliberately simple: users sign up, enter a keyword, and
launch an AI agent that searches the web and ranks the results by publication
date. The value is everything around it — the patterns you would otherwise
rebuild from scratch on every agent project.

## What you get

- **Hexagonal architecture on both server bricks** — pure domains with zero
  infrastructure dependencies; use cases depend on ports (Rust traits / Python
  Protocols); adapters implement them. Swapping the LLM, the search provider,
  or the database is configuration, not surgery.
- **TDD throughout** — 85+ tests; the domain is tested with fakes of the ports,
  and **no test ever calls a paid service** (live provider tests are opt-in
  behind `RUN_LIVE_TESTS=1`).
- **Reliable job lifecycle** — `pending → running → completed/failed` with a
  timeout reaper for stuck jobs, Celery retries with backoff, and end-to-end
  idempotence (safe re-delivery, no duplicates).
- **Real auth** — argon2id passwords, short-lived JWT access tokens, single-use
  refresh tokens (SHA-256-hashed at rest, rotated on every refresh, HttpOnly
  cookie), silent session restore on page reload.
- **Abuse protection** — per-user daily search quota (LLM calls cost money) and
  per-IP rate limiting on auth and API routes.
- **Observability** — `X-Request-Id` correlation propagated across all four
  processes, structured JSON logs behind a `LOG_FORMAT` switch.
- **Fully containerized** — multi-stage Dockerfiles, one compose file for dev
  (infra-only or full profile) and a production override with Caddy/TLS.
- **CI** — GitHub Actions: lint + test on every PR, images published to GHCR
  on `main`, and weekly security audits (cargo/pip/npm audit + gitleaks). A
  GitLab CI mirror (`.gitlab-ci.yml`) ships for GitLab-hosted forks, including
  a reference VPS deploy job.
- **Every decision written down** — 22 Architecture Decision Records in
  [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), including the rejected
  alternatives and the trade-offs.

## Architecture

```
Vue SPA ──▶ Rust API (Axum) ──▶ FastAPI micro-API ──▶ Redis ──▶ Celery worker
 (JWT)        │    ▲                (Celery client)              (LangChain agent)
              ▼    │                                              1. web search (Tavily)
         PostgreSQL└──── HTTP callbacks (started/results/failure) 2. date extraction (Claude)
                                                                  3. sort by publication date
```

The worker never touches the database — results flow back through
authenticated HTTP callbacks, so a single application owns the schema
([ADR-006](docs/ARCHITECTURE.md)).

## Stack

| Brick | Tech |
|---|---|
| `backend/` | Rust, Axum, sqlx — web API, accounts, job orchestration (hexagonal) |
| `agent/` | Python, LangChain (Claude) + Celery — research agent (hexagonal), FastAPI micro-API |
| `frontend/` | Vue 3, Vite, Pinia — SPA with silent token refresh |
| Infra | PostgreSQL 16, Redis 7, Docker, Caddy, GitHub Actions (GitLab CI mirror) |

## Quick start

Prerequisites: Docker, Rust, [uv](https://docs.astral.sh/uv), Node 22.
API keys: [Anthropic](https://console.anthropic.com) and
[Tavily](https://app.tavily.com) (free tier).

```sh
cp .env.example .env          # fill in ANTHROPIC_API_KEY and TAVILY_API_KEY

docker compose up -d          # infra only: PostgreSQL + Redis

# Terminal 1 — Rust backend (http://localhost:8000)
cd backend && cargo run

# Terminal 2 — agent micro-API (http://localhost:8001)
cd agent && uv sync && uv run uvicorn aiagent.adapters.api.app:app --port 8001

# Terminal 3 — Celery worker
cd agent && uv run celery -A aiagent.celery_app worker --loglevel=info

# Terminal 4 — frontend (http://localhost:5173)
cd frontend && npm install && npm run dev
```

Or run the fully containerized stack (what CI builds):

```sh
docker compose --profile full up --build   # frontend on http://localhost:8080
```

## Tests

```sh
cd backend && cargo test         # domain + use cases with port fakes; +Postgres tests when DATABASE_URL is set
cd agent && uv run pytest        # domain + use cases, Celery in eager mode
cd frontend && npm test          # vitest + Vue Test Utils
```

## Repository layout

```
backend/src/domain/             # entities + ports (traits) — no infrastructure deps
backend/src/application/        # use cases, unit-tested with fakes
backend/src/adapters/           # http (axum), persistence (sqlx/in-memory), auth, dispatch
agent/src/aiagent/domain/       # results, date normalization, sorting + ports (Protocols)
agent/src/aiagent/application/  # run_research use case (date cascade)
agent/src/aiagent/adapters/     # tavily, llm (Claude), sink (callbacks), api (FastAPI)
frontend/src/                   # Vue 3 SPA
docs/                           # ARCHITECTURE.md (22 ADRs), COMMANDS.md, diagrams/
```

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — every technical decision
  (ADR-001 → ADR-022), kept in sync with the code at all times.
- [docs/COMMANDS.md](docs/COMMANDS.md) — every dev/test/deploy command.
- [docs/diagrams/](docs/diagrams/) — PlantUML sequence diagrams (auth flow).
- [TODO.md](TODO.md) — manual setup checklist (CI, VPS, API keys) and the
  prioritized technical roadmap.

## Deployment (for your fork)

This repository is source code only — **it deploys nothing itself**. Fork it
and deploy on your own infrastructure: everything is provided for a
single-VPS setup with docker compose behind Caddy (automatic TLS) —
production compose override, step-by-step provisioning checklist
([TODO.md](TODO.md) §4), and a reference CI deploy job in the GitLab mirror.
Reproducible by design: images are pinned to the commit SHA, and a rollback is
redeploying the previous tag. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
(ADR-015/019).

## Contributing

Fork → short-lived branch → PR against `main`. CI must be green; PRs are
squash-merged. See [CONTRIBUTING.md](CONTRIBUTING.md) for the ground rules
(English only, TDD, architecture doc kept in sync).

## License

MIT — see [LICENSE](LICENSE).
