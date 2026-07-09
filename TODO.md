# TODO — manual setup checklist

Everything that must be done by hand (GitHub, VPS, API accounts) before the
pipeline and the deployment work end to end. Tick as you go.

## 1. Local repository

- [x] Align the local repo on GitHub Flow (ADR-019): `master` renamed to
      `main`, `develop` deleted, gitflow config removed.
- [x] First commit on `main` + push to GitHub — done (2026-07-09, full
      verification green beforehand: 101 tests across the three bricks).
- [ ] `cp .env.example .env` and fill in `ANTHROPIC_API_KEY` + `TAVILY_API_KEY`
      (local development only — never committed).

## 2. API provider accounts

- [ ] Anthropic: create an API key on https://console.anthropic.com (billing enabled).
- [ ] Tavily: create an API key on https://app.tavily.com (free tier is enough to start).

## 3. GitHub repository (ADR-019)

- [ ] Fill in the repo **description** and add topics (`rust`, `axum`,
      `langchain`, `celery`, `vuejs`, `hexagonal-architecture`, `ai-agents`,
      `boilerplate`).
- [ ] Push `main` and check the first `CI` workflow run: lint + test green for
      the three bricks; image builds succeed (not pushed on PRs).
- [ ] **Branch protection on `main`** (Settings → Rules → Rulesets, or
      Branches): require a pull request (1 approval, dismiss stale approvals),
      require status checks — select `backend (lint + test)`,
      `agent (lint + test)`, `frontend (lint + test)` — require conversation
      resolution, block force pushes. Grant yourself bypass on the approval
      rule if you maintain solo.
- [ ] **Merge methods** (Settings → General → Pull Requests): enable **squash
      merge only** + "Automatically delete head branches".
- [ ] Check the **Security audits** workflow: it runs every Monday 06:00 UTC
      automatically; trigger it once manually (Actions → Security audits →
      Run workflow) to validate.
- [ ] GHCR images are published under
      `ghcr.io/christopheduc-me/ai-agent-boilerplate/*`: after the first `main`
      push, set the packages' visibility to **public** (Package settings) so
      anyone can pull and try the stack without auth.

### Optional — GitLab mirror

`.gitlab-ci.yml` mirrors the same pipeline for GitLab users (not executed on
GitHub). If you also host on GitLab: protect `main`, add the same CI/CD
variables, and create the weekly audit schedule (`0 6 * * 1`) there.

## 4. VPS provisioning (ADR-015) — for your own deployment/fork

The boilerplate repository itself deploys nothing (ADR-019). This checklist is
the deployment story for **your fork** or private instance. Automation options:
wire your own deploy job (the GitLab CI mirror contains a reference
`deploy:vps` job to copy), or deploy by hand with the commands in
docs/COMMANDS.md §10.

- [ ] Rent a VPS (2 vCPU / 4 GB RAM is comfortable for the full stack).
- [ ] Install Docker Engine + the compose plugin (https://docs.docker.com/engine/install/).
- [ ] Create a non-root deploy user, member of the `docker` group:
  ```sh
  adduser deploy && usermod -aG docker deploy
  ```
- [ ] *(only if you wire a CI deploy job in your fork)* Generate a dedicated
      SSH keypair and authorize it:
  ```sh
  ssh-keygen -t ed25519 -f ci_deploy_key -N ""
  # public key  -> /home/deploy/.ssh/authorized_keys on the VPS
  # private key -> your CI's secret store (e.g. SSH_PRIVATE_KEY)
  ```
- [ ] Firewall: allow only 22/80/443 inbound (e.g. `ufw allow 22,80,443/tcp && ufw enable`).
- [ ] DNS: point an A record of your domain to the VPS IP.
- [ ] Create `/opt/aiagent/` owned by `deploy` and copy into it:
  - [ ] `docker-compose.yml`
  - [ ] `docker-compose.prod.yml`
  - [ ] `Caddyfile` — **replace `example.com` with the real domain**
  - [ ] `.env` (production) with strong values, written once by hand:
    ```
    INTERNAL_API_TOKEN=<openssl rand -hex 32>
    JWT_SECRET=<openssl rand -hex 32>
    ANTHROPIC_API_KEY=...
    TAVILY_API_KEY=...
    ```
- [ ] Registry access for the VPS: if the GHCR packages are **public** (step 3),
      nothing to do. If private, create a fine-grained PAT with `read:packages`
      and log in:
  ```sh
  docker login ghcr.io -u christopheduc-me -p <PAT-read-packages>
  ```
- [ ] First deployment (by hand, or via your fork's deploy job — see
      docs/COMMANDS.md §10 for the exact commands), then check:
  ```sh
  curl https://<domain>/healthz        # via Caddy -> backend
  docker compose -f docker-compose.yml -f docker-compose.prod.yml --profile full ps
  ```
- [ ] Set up the daily PostgreSQL backup cron (see docs/COMMANDS.md §10), e.g.:
  ```
  0 3 * * * cd /opt/aiagent && docker compose -f docker-compose.yml -f docker-compose.prod.yml exec -T postgres pg_dump -U app aiagent | gzip > /opt/aiagent/backups/aiagent_$(date +\%F).sql.gz
  ```
  - [ ] Create `/opt/aiagent/backups/` and add a retention rule (e.g. `find ... -mtime +14 -delete`).

## 5. Technical roadmap (prioritized)

Deliberate scope cuts and hardening steps, ordered by risk. The ports are in
place — each item is an adapter/use-case cycle away.

### P1 — Core reliability (before real usage)

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

### P2 — Operability

- [x] **End-to-end correlation (ADR-018)** — done: `X-Request-Id` middleware on
      the Rust API, `job_id` propagated Rust → FastAPI → Celery → callbacks,
      `LOG_FORMAT=json` structured logs on all three server processes
      (enabled in `docker-compose.prod.yml`).
- [x] **Security hygiene in CI (ADR-015 amendment)** — done: `audit` stage with
      `cargo audit`, `pip-audit`, `npm audit`, gitleaks; runs on the weekly
      schedule only (creation of the schedule: §3 above).

### P3 — Agent product quality

- [ ] **Date cascade stage 2 (ADR-011)**: fetch the page and read JSON-LD
      `datePublished` / OpenGraph before falling back to the LLM — cheaper and
      `high` confidence instead of `medium`.
- [ ] **URL normalization + deduplication** in the agent domain (tracking params
      make the same article count twice today).
- [ ] Optional `RUN_LIVE_TESTS=1` integration tests for the Tavily and Claude
      adapters (ADR-012).

### P4 — Comfort (later)

- [x] **E2E smoke test on the full compose stack in CI (ADR-021)** — done:
      deterministic fake providers (`AGENT_PROVIDERS=fake`, keyless),
      `scripts/e2e-smoke.sh` through nginx, `e2e` job in GitHub Actions and
      the GitLab mirror. (Playwright browser-level tests remain a possible
      upgrade.)
- [x] **Dependency freshness without a platform bot (ADR-022)** — done:
      `scripts/deps-report.sh` (native tools) run weekly by both CIs, plus an
      inert portable `renovate.json` for forks that want automated update PRs
      (connect the Mend app on GitHub, or a scheduled renovate container job
      on GitLab/self-hosted, to activate it).
- [ ] SSE to replace frontend polling (noted in ARCHITECTURE §5).
- [ ] Code coverage reporting in CI; pre-commit hooks (lefthook).
- [ ] Redis-backed rate limiter if the backend ever scales horizontally (ADR-017).
