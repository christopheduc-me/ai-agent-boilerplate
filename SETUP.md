# SETUP — running and deploying the stack

Everything that must be done by hand (local env, API accounts, GitHub, VPS)
to run the stack locally and deploy it. Some steps are one-time, others apply
to every new machine or deployment — tick as you go. The technical roadmap
lives in [ROADMAP.md](ROADMAP.md).

## 1. Local repository

- [x] Align the local repo on GitHub Flow (ADR-019): `master` renamed to
      `main`, `develop` deleted, gitflow config removed.
- [x] First commit on `main` + push to GitHub — done (2026-07-09, full
      verification green beforehand: 101 tests across the three bricks).
- [ ] `cp .env.example .env` and fill in `ANTHROPIC_API_KEY` + `TAVILY_API_KEY`
      (no Anthropic key needed with a local model: `AGENT_LLM_BACKEND=ollama`,
      see ADR-041 and `docs/COMMANDS.md` §“Local LLM”)
      (local development only — never committed). No keys yet? Set
      `AGENT_PROVIDERS=fake` instead to run the whole stack keyless with
      deterministic results (ADR-021).

Then launch: dev mode (compose infra + the three bricks by hand) or the fully
containerized stack — the exact commands are in the README Quick start and
docs/COMMANDS.md.

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
- [ ] **Configure Codecov** (ADR-023): on https://app.codecov.io, open the
      repo's configuration and copy the **repository upload token**, then add
      it as a GitHub Actions secret named `CODECOV_TOKEN` (Settings → Secrets
      and variables → Actions). The badge and PR diff-coverage comments start
      working from the next CI run. Safe in a public repo: Actions secrets are
      never exposed to fork PRs.
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
  - [ ] the `deploy/` directory as-is (keep the subdirectory — the compose
        paths assume it):
    - [ ] `deploy/docker-compose.prod.yml`
    - [ ] `deploy/Caddyfile` — **replace `example.com` with the real domain**
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
  docker compose -f docker-compose.yml -f deploy/docker-compose.prod.yml --profile full ps
  ```
- [ ] Set up the daily PostgreSQL backup cron (see docs/COMMANDS.md §10), e.g.:
  ```
  0 3 * * * cd /opt/aiagent && docker compose -f docker-compose.yml -f deploy/docker-compose.prod.yml exec -T postgres pg_dump -U app aiagent | gzip > /opt/aiagent/backups/aiagent_$(date +\%F).sql.gz
  ```
  - [ ] Create `/opt/aiagent/backups/` and add a retention rule (e.g. `find ... -mtime +14 -delete`).
