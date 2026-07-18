# Project conventions

## Language

Everything committed to this repository — documentation, code, comments, commit
messages, identifiers — is written in **English only**.

## Architecture document is the single source of truth

`docs/ARCHITECTURE.md` records every technical decision (ADR-001 to ADR-039)
and MUST stay in sync with the code at all times:

- Any change that affects the architecture (new adapter, new dependency,
  changed API contract, changed infrastructure, port/schema change, revisited
  decision) updates `docs/ARCHITECTURE.md` **in the same change**.
- A revisited decision gets a new ADR entry; never rewrite history.
- Not-yet-implemented parts are marked *(planned)* in the document and tracked
  in `ROADMAP.md`. When implementing one, flip both.
- Before starting any structural work, read the relevant ADRs first.

## Architecture rules (summary — details in docs/ARCHITECTURE.md)

- Hexagonal on both server bricks: `domain/` has zero infrastructure
  dependencies; use cases depend on ports (Rust traits / Python Protocols);
  adapters implement them.
- The agent worker never touches the database — results go through the HTTP
  callback to the Rust API (ADR-006).
- TDD: test first; unit tests use fakes of the ports; no test may call a paid
  service (Anthropic, Tavily) — live tests only behind `RUN_LIVE_TESTS=1`.

## Commands

All dev/test/deploy commands are listed in `docs/COMMANDS.md`. Quick check
before committing:

```sh
cd backend && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cd agent && uv run ruff check . && uv run ruff format --check . && uv run mypy src && uv run pytest
cd frontend && npm run lint && npm run typecheck && npm test
```

PostgreSQL integration tests need the compose service:
`docker compose up -d postgres` then `DATABASE_URL=postgres://app:app@localhost:5433/aiagent cargo test`.
