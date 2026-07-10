# Contributing

Thanks for considering a contribution! This project is a boilerplate, so the
bar is on **clarity and consistency of the patterns**, not on feature count.

## Workflow

1. **Fork** the repository and create a short-lived branch from `main`
   (`feat/...`, `fix/...`, `docs/...`).
2. Make your change following the conventions below.
3. Open a **pull request against `main`**. CI (lint + tests for the three
   bricks) must be green; PRs are squash-merged.

## Ground rules

- **English only** — code, comments, identifiers, commit messages, docs.
- **TDD** — no behavior change without a test. Domain and use cases are tested
  with fakes of the ports; **no test may call a paid service** (Anthropic,
  Tavily). Live tests go behind `RUN_LIVE_TESTS=1` (ADR-012).
- **Architecture doc stays in sync** — any change that affects the architecture
  (new adapter, new dependency, changed API contract, changed infrastructure,
  revisited decision) must update [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
  **in the same PR**. A revisited decision gets a new ADR entry; never rewrite
  history.
- **Hexagonal discipline** — `domain/` has zero infrastructure dependencies;
  use cases depend on ports (Rust traits / Python Protocols); adapters
  implement them. The agent worker never touches the database (ADR-006).

## Before pushing

Optional but recommended — install the pre-commit hooks (fast lint/format
checks on every commit; `git commit --no-verify` bypasses them):

```sh
brew install lefthook   # or see https://lefthook.dev for other platforms
lefthook install
```

Run the same checks CI runs:

```sh
cd backend && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cd agent && uv run ruff check . && uv run ruff format --check . && uv run mypy src && uv run pytest
cd frontend && npm run lint && npm run typecheck && npm test
```

PostgreSQL integration tests need the compose service:

```sh
docker compose up -d postgres
DATABASE_URL=postgres://app:app@localhost:5433/aiagent cargo test
```

Every dev command is listed in [docs/COMMANDS.md](docs/COMMANDS.md).

## Reporting bugs / proposing features

Use the issue templates. For security vulnerabilities, please do **not** open
a public issue — contact the maintainer directly.
