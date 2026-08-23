# Security policy

## Reporting a vulnerability

**Please do not open a public issue.**

Use GitHub's [private vulnerability
reporting](https://github.com/christopheduc-me/ai-agent-boilerplate/security/advisories/new)
— it is the preferred channel, it keeps the discussion private until a fix
exists, and it produces the advisory for you. If you would rather use email:
**github@christopheduc.me**.

Useful things to include, in rough order of value: what an attacker gains, the
smallest reproduction you have, the affected brick (`backend/`, `agent/`,
`frontend/`, or the deployment), and the commit you tested. A proof of concept
against a local `docker compose --profile full` stack is ideal.

This is a single-maintainer project, so no response time is promised. Reports
are read and acknowledged as soon as they are seen; you will be credited in the
advisory unless you ask otherwise.

## Supported versions

There are **no release branches and no published versions** — this is a
boilerplate you fork, not a dependency you install. Fixes land on `main`, and
a fork picks them up by merging (see the sync section in
[docs/FORKING.md](docs/FORKING.md)).

If you run a fork in production, treat `main` as the only supported line.

## What is *not* a vulnerability here

The repository ships a **runnable example**, and two of its properties look
alarming to a scanner while being deliberate:

- **Placeholder secrets in `.env.example`** (`JWT_SECRET=change-me`,
  `INTERNAL_API_TOKEN=change-me`). They are a template, not credentials, and
  they cannot reach production silently: with `APP_ENV=production` the backend
  refuses to start when a required variable is missing, empty, or still a
  development placeholder (ADR-020).
- **The keyless demo mode** (`AGENT_PROVIDERS=fake`, ADR-021). It exists so the
  stack, the e2e suite and CI run without paid API keys. It is deterministic and
  calls nothing external.

Findings in a **dependency** with no fix available upstream are also out of
scope for a report here — the weekly Trivy scan already ignores unfixed
findings on purpose (ADR-074), because a policy that fails on things nobody can
act on is one people learn to ignore. If you know of a fix we have missed, that
*is* worth reporting.

## What the project already does

Context for a reporter, not a claim of completeness — the details are in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md):

- Argon2 password hashing, single-use refresh-token rotation with reuse
  detection and family revocation (ADR-008/056).
- Per-IP and per-account rate limiting, a per-user quota, and an append-only
  security audit log with alertable metrics (ADR-017/057/060/068).
- SSRF guards on user-supplied webhook URLs, including a connect-time DNS
  resolver that refuses non-public addresses (ADR-055/056).
- A spend cap and step budget bounding a runaway agent (ADR-030/048).
- Weekly CI audits: `cargo audit`, `pip-audit`, `npm audit`, gitleaks, and
  Trivy image scans (ADR-015 amendment).

Known accepted risks are recorded as ADRs with their justification rather than
left implicit — `backend/.cargo/audit.toml` is the one place an advisory may be
ignored, and each entry there carries a written reason.
