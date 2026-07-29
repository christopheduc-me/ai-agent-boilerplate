# Commands Cheat Sheet

Every useful command for developing, testing, running, and deploying the project.
All commands are run from the repository root unless stated otherwise.

## 0. One-time setup

```sh
cp .env.example .env      # then fill in ANTHROPIC_API_KEY and TAVILY_API_KEY

# Toolchains (macOS): Rust, uv (Python), Node 22
# rustup: https://rustup.rs — uv: https://docs.astral.sh/uv — node: nvm install 22

cd agent && uv sync            # creates agent/.venv from uv.lock
cd frontend && npm install    # creates frontend/node_modules
```

---

## 1. Docker — full stack

```sh
# Build and start everything (backend, agent API, worker, frontend, PostgreSQL, Redis)
docker compose --profile full up --build

# Same, detached
docker compose --profile full up --build -d

# Stop everything (keeps data volumes)
docker compose --profile full down

# Stop everything AND wipe the PostgreSQL volume
docker compose --profile full down -v
```

Entry points once up:

| Service | URL |
|---|---|
| Frontend (nginx) | http://localhost:8080 |
| Rust backend API | http://localhost:8000 (healthz: `/healthz`) |
| API docs (Swagger UI) | http://localhost:8000/api/docs (raw spec: `/api/openapi.json`) — ADR-049 |
| Agent FastAPI | http://localhost:8001 (healthz: `/healthz`) |
| PostgreSQL | localhost:5433 (`app`/`app`, db `aiagent`) |
| Redis | localhost:6379 |

With `--profile observability` (also linked from the workbench's "Ops
consoles" card, ADR-040):

| Console | URL |
|---|---|
| Flower (Celery workers & tasks) | http://localhost:5555 |
| Jaeger (distributed traces) | http://localhost:16686 |
| Prometheus (metrics — ADR-050) | http://localhost:9090 |
| Grafana (dashboards — ADR-050) | http://localhost:3001 |

## 2. Docker — infra only (default profile)

The recommended dev mode: containers for PostgreSQL + Redis, everything else local
with hot-reload.

```sh
docker compose up -d          # postgres + redis only
docker compose down           # stop them
```

## 3. Docker — start or inspect a single service

```sh
# Start one service (add --profile full for app services)
docker compose up -d postgres
docker compose up -d redis
docker compose --profile full up -d --build backend
docker compose --profile full up -d --build agent-api agent-worker
docker compose --profile full up -d --build frontend

# Rebuild a single image without starting it
docker compose --profile full build backend

# Restart a single service
docker compose --profile full restart agent-worker

# Logs (follow)
docker compose --profile full logs -f backend
docker compose --profile full logs -f agent-worker

# Status + health of all services
docker compose --profile full ps

# Shell inside a running container
docker compose --profile full exec backend sh
docker compose --profile full exec postgres psql -U app -d aiagent
docker compose --profile full exec redis redis-cli
```

## 4. Local development (hot-reload)

Requires `docker compose up -d` (infra) first. One terminal per service:

```sh
# Rust backend — http://localhost:8000
cd backend
cargo run
# Reads ../.env automatically (dotenvy). Without DATABASE_URL it falls back to
# in-memory persistence; without AGENT_API_URL jobs are accepted but not
# dispatched (noop dispatcher).

# Agent FastAPI micro-API — http://localhost:8001
cd agent
uv run uvicorn aiagent.adapters.api.app:app --port 8001 --reload

# Celery worker
cd agent
uv run celery -A aiagent.celery_app worker --loglevel=info

# Frontend — http://localhost:5173 (proxies /api to :8000)
cd frontend
npm run dev
```

---

## 5. Tests

### All test suites (what CI runs)

```sh
cd backend && cargo test
cd agent && uv run pytest
cd frontend && npm test
```

### Backend (Rust)

```sh
cd backend
cargo test                                  # everything (unit + integration)
cargo test --lib                            # unit tests only
cargo test --test http_api                  # the HTTP integration test file
cargo test full_search_lifecycle            # a single test by name
cargo test domain::                         # all domain tests
cargo test -- --nocapture                   # show stdout/tracing while testing

# OpenAPI docs (ADR-049): browsable at /api/docs. Regenerate the committed spec
# after changing a public endpoint or DTO (a drift test enforces it):
cargo run --example openapi > ../docs/openapi.json
```

### Agent (Python)

```sh
cd agent
uv run pytest                               # everything
uv run pytest tests/test_run_research.py    # one file
uv run pytest -k cascade                    # by keyword
uv run pytest -x -vv                        # stop at first failure, verbose
# Live provider tests (ADR-012): call the PAID Tavily/Claude APIs — opt-in,
# needs real keys in the repo-root .env, never run in CI. Use after bumping
# AGENT_MODEL_ID or when extraction quality degrades (provider drift check).
set -a && source ../.env && set +a
RUN_LIVE_TESTS=1 uv run pytest tests/test_live_providers.py -v
```

### Frontend (Vue)

```sh
cd frontend
npm test                                    # single run (CI mode)
npx vitest                                  # watch mode
npx vitest run src/components/__tests__/ResultTimeline.spec.ts   # one file
```

### End-to-end (full compose stack, no API key — ADR-021)

```sh
# Boot the fully containerized stack with the deterministic fake providers
echo "AGENT_PROVIDERS=fake" >> .env        # or export it in the shell
echo "RATE_LIMIT_AUTH_PER_MINUTE=100" >> .env  # e2e registers several accounts/minute (ADR-017)
echo "SCHEDULER_TICK_SECONDS=5" >> .env        # fast recurring-search runs (ADR-033)
docker compose --profile full up -d --build --wait

scripts/e2e-smoke.sh                        # register -> login -> search -> results
scripts/e2e-smoke.sh http://other-host:8080 # any base URL

docker compose --profile full down          # teardown (remember to revert .env)
```

`AGENT_PROVIDERS=fake` also works for keyless local development (the worker
starts without ANTHROPIC/TAVILY keys and returns deterministic results).

### Local LLM (Ollama — ADR-041)

Run the live agent against a model on your own machine instead of the
Anthropic API (Tavily stays required for the searches):

```sh
ollama pull qwen3:14b                       # once; any instruct model works
# In .env (or exported): the backend, the local model, zero LLM cost rates
AGENT_LLM_BACKEND=ollama
AGENT_MODEL_ID=qwen3:14b
LLM_COST_INPUT_PER_MTOK=0
LLM_COST_OUTPUT_PER_MTOK=0
# Local bricks reach it at localhost (the default); compose containers use
# the preconfigured http://host.docker.internal:11434 automatically.

# Opt-in drift check of the local model (mirrors the ADR-012 live tests)
cd agent
RUN_OLLAMA_TESTS=1 AGENT_LLM_BACKEND=ollama AGENT_MODEL_ID=qwen3:14b \
  uv run pytest tests/test_live_ollama.py -v
```

### Compare models — evaluation harness (ADR-045)

Answer "which model is good enough?" — score any backend/model on the agent's
three LLM capabilities and print a comparison table. Calls real providers
(paid for Anthropic, free for local Ollama), so run it by hand, never in CI.

```sh
cd agent
# Compare several models side by side; specs are `backend:model_id`
uv run python -m aiagent.evaluation \
  ollama:gemma4:latest ollama:qwen3:14b anthropic:claude-opus-4-8
# No args -> evaluate the .env-configured backend/model
uv run python -m aiagent.evaluation
# -v prints every case's score and detail
uv run python -m aiagent.evaluation -v ollama:gemma4:latest
# Pre-release quality gate: exit non-zero if overall drops below a floor
uv run python -m aiagent.evaluation --fail-under 0.8 anthropic:claude-opus-4-8
```

The table shows per-capability scores (enrichment / policy / critic),
overall, total latency and indicative cost (0 for local). It is a directional
signal, not a benchmark — extend the golden cases in `aiagent/evaluation.py`
for your own domain.

**Pre-release ritual (the regression net for the non-deterministic part).**
Unit tests use port fakes (ADR-012), so they never exercise the real prompts or
model — a prompt edit, a model bump, or a LangChain/LangGraph upgrade can pass
CI green and still degrade the agent. Before shipping such a change, run the
harness live with `--fail-under`: it prints the table and exits non-zero if any
model's overall score is under the floor, turning "eyeball the numbers" into a
clear PASS/FAIL. It stays **local, not CI** — keeping API keys out of the
repo's CI is the deliberate trade-off (ADR-045).

### Browser tests (Playwright — ADR-028)

Same stack as the smoke script (boot it first, see above), driven through a
real Chromium:

```sh
cd frontend
npx playwright install chromium             # one-time browser download
npm run test:e2e                            # register -> search -> timeline
E2E_BASE_URL=http://other-host:8080 npm run test:e2e   # any base URL
npx playwright show-report                  # inspect a failed run
```

### Worker console (Flower — ADR-040, opt-in)

```sh
# Live view of the Celery workers: tasks, retries, failures — no key needed
docker compose --profile observability up -d flower
open http://localhost:5555
```

### Traces + metrics (OpenTelemetry — ADR-029/050, opt-in)

The observability profile runs an OTel Collector (the OTLP entry point) that
fans traces to Jaeger and metrics to Prometheus, with Grafana for dashboards.

```sh
# Full stack + the observability collector/Jaeger/Prometheus/Grafana
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4318 \
  docker compose --profile full --profile observability up -d --build --wait
open http://localhost:16686                 # Jaeger (traces)
open http://localhost:3001                  # Grafana (metrics dashboards)

# Hot-reload dev: run the observability stack in Docker, point local bricks at it
docker compose --profile observability up -d otel-collector jaeger prometheus grafana
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 cargo run   # same var for the agent
```

Unset (or empty), the variable disables telemetry entirely — the default. Logs
carry the `trace_id` when tracing is on, so a log line links to its Jaeger
trace. What to watch, the metrics and their PromQL: [OBSERVABILITY.md](OBSERVABILITY.md).

---

## 6. Lint / format / typecheck (same as the CI `lint` stage)

```sh
# Backend
cd backend
cargo fmt                                   # format
cargo fmt --check                           # check only (CI)
cargo clippy --all-targets -- -D warnings   # lint (CI)

# Agent
cd agent
uv run ruff format .                        # format
uv run ruff format --check .                # check only (CI)
uv run ruff check .                         # lint (CI)
uv run ruff check --fix .                   # lint + autofix
uv run mypy src                             # typecheck (CI)

# Frontend
cd frontend
npm run lint                                # eslint (CI)
npm run typecheck                           # vue-tsc (CI)
```

---

## 7. Builds

```sh
# Release binaries (Rust)
cd backend && cargo build --release         # target/release/backend + healthcheck

# Production bundle (Vue)
cd frontend && npm run build                # dist/

# Docker images, exactly as CI builds them
docker build -t aiagent/backend ./backend
docker build -t aiagent/agent ./agent
docker build -t aiagent/frontend ./frontend

# Diagrams: regenerate the committed SVG renders after editing a .puml
# (index with all renders: docs/diagrams/README.md)
plantuml -tsvg docs/diagrams/*.puml
```

---

## 8. Quick API smoke test (curl)

With the backend running on :8000:

```sh
# Health
curl http://localhost:8000/healthz

# Register
curl -X POST http://localhost:8000/api/auth/register \
  -H 'content-type: application/json' \
  -d '{"email":"me@example.com","password":"password123"}'

# Login -> capture the token
TOKEN=$(curl -s -X POST http://localhost:8000/api/auth/login \
  -H 'content-type: application/json' \
  -d '{"email":"me@example.com","password":"password123"}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')

# Launch a search
curl -X POST http://localhost:8000/api/searches \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"keyword":"rust hexagonal architecture"}'

# List searches / read one
curl -H "authorization: Bearer $TOKEN" http://localhost:8000/api/searches
curl -H "authorization: Bearer $TOKEN" http://localhost:8000/api/searches/<job_id>
```

---

## 9. Dependency management

```sh
# Backend
cd backend && cargo update                  # refresh Cargo.lock
cd backend && cargo add <crate>

# Agent
cd agent && uv add <package>                # add a dependency
cd agent && uv add --group dev <package>    # add a dev dependency
cd agent && uv lock --upgrade               # refresh uv.lock

# Frontend
cd frontend && npm install <package>
cd frontend && npm update

# Outdated report across the three bricks (native tools, no bot — ADR-022);
# also run weekly by both CIs alongside the security audits
scripts/deps-report.sh            # or: scripts/deps-report.sh backend|agent|frontend

# Security audits, exactly as the weekly CI runs them (ADR-015 amendment)
cd backend && cargo audit         # exceptions: backend/.cargo/audit.toml (justified)
cd agent && uv export --frozen --no-emit-project -o /tmp/req.txt && uvx pip-audit -r /tmp/req.txt
cd frontend && npm audit --audit-level=high
```

`cargo audit` scans the lockfile, which lists optional dependencies that are
never compiled (e.g. the unused MySQL driver). Any advisory ignored in
`backend/.cargo/audit.toml` must carry a written justification — see the
ADR-015 amendment in docs/ARCHITECTURE.md.

---

## 10. CI/CD and production (VPS)

**GitHub Actions is the primary CI** (ADR-019, `.github/workflows/`): `ci.yml`
runs lint + tests on every PR and publishes images to GHCR on `main`;
`security.yml` runs the weekly audits (Monday 06:00 UTC; on-demand via
Actions → Run workflow). **The boilerplate repo deploys nothing** — deployment
is your fork's business; `.gitlab-ci.yml` mirrors the pipeline for
GitLab-hosted forks and includes a reference `deploy:vps` job.

On the VPS (`/opt/aiagent/`, by hand or from your fork's deploy job):

```sh
export CI_REGISTRY_IMAGE=ghcr.io/christopheduc-me/ai-agent-boilerplate IMAGE_TAG=<short_sha>

docker compose -f docker-compose.yml -f deploy/docker-compose.prod.yml --profile full pull
docker compose -f docker-compose.yml -f deploy/docker-compose.prod.yml --profile full up -d

# Rollback = redeploy the previous tag
IMAGE_TAG=<previous_sha> docker compose -f docker-compose.yml -f deploy/docker-compose.prod.yml --profile full up -d

# Logs / status in production
docker compose -f docker-compose.yml -f deploy/docker-compose.prod.yml --profile full ps
docker compose -f docker-compose.yml -f deploy/docker-compose.prod.yml --profile full logs -f backend

# Manual database backup (a daily cron does this, see ADR-015)
docker compose -f docker-compose.yml -f deploy/docker-compose.prod.yml exec postgres \
  pg_dump -U app aiagent > backup_$(date +%F).sql
```
