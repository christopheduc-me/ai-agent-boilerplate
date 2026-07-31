# Make it yours

This boilerplate ships one **working example** — a chronological web-research
agent (keyword → search → enrich → timeline). The point is to keep the
load-bearing infrastructure (auth, the job queue, the HTTP callback contract,
human-in-the-loop, SSE, the spend cap, observability, deployment) and **swap the
example domain for yours at the hexagonal seams**. This guide is the shortest
path there.

New to the codebase? Read the argument first: [ARCHITECTURE.md](ARCHITECTURE.md)
§1–2 and the [hexagonal diagram](diagrams/README.md). The seams below are the
ports it describes.

## The mental model

The agent's *task* lives behind a handful of **ports** (Python `Protocol`s). The
loop/graph that drives them (ADR-030/046), the queue, the callbacks, and the UI
plumbing are task-agnostic. To change what the agent *does*, you reshape the
ports, their domain types, and the prompts — not the orchestration.

```
your task  =  domain types + ports (what)  +  adapters (how)  +  prompts
kept as-is =  the agentic loop, jobs, auth, callbacks, HITL, SSE, metrics, deploy
```

## 1. Swap the domain (the real work)

Everything task-specific is in three files on the agent side:

| File | What to change |
|---|---|
| `agent/src/aiagent/domain/models.py` | The domain types: `RawSearchHit`, `HitEnrichment`, `ResearchResult` (the example's "a dated web result"). Reshape them into *your* input/output — e.g. a support ticket, a code diff, a lead. Keep `SearchAction`/`AskAction`/`FinishAction`/`AgentStep` (the loop's vocabulary) unless you change the control flow. |
| `agent/src/aiagent/domain/ports.py` | The `Protocol`s the agent calls: `SearchProvider` (gather), `HitEnricher` (analyze), `AgentPolicy` (decide next action), `ResultCritic` (review). Rename/reshape them to your task's verbs. |
| `agent/src/aiagent/adapters/llm.py` | The prompts (`ENRICHMENT_PROMPT`, `POLICY_PROMPT`, `CRITIQUE_PROMPT`) and their pydantic reply schemas (`EnrichmentReply`, `ActionReply`, `CritiqueReply`). This is where you teach the LLM *your* task. |

The orchestration in `application/run_agent_research.py` and
`adapters/orchestration/langgraph_agent.py` drives those ports generically —
you usually leave it alone. Its unit tests (`tests/test_run_agent_research.py`,
`tests/test_langgraph_agent.py`) use scripted fakes of the ports: update the
fakes, keep the TDD.

## 2. Swap the adapters (how)

Each port has a **production adapter** and a **keyless fake** (ADR-021). Replace
the production one; keep a fake so CI and the demo stay keyless.

| Port | Production adapter | Swap it for |
|---|---|---|
| `SearchProvider` | `adapters/tavily.py` (or keyless `duckduckgo.py`) | your tool/API/DB — implement `search()`. Providers *compose*: `AggregatingSearchProvider` (ADR-051) fans a query across several and fuses the results — a worked example of the port paying off |
| `HitEnricher` / `AgentPolicy` / `ResultCritic` | `adapters/llm.py` | usually kept (they're LLM-generic); just change prompts/schemas |
| `ResultSink` / `StepReporter` | `adapters/sink.py` | kept — it POSTs to the Rust backend (ADR-006). If your result shape changes, update `serialize_result` **and its contract fixture** (below) |

The **composition root** is `agent/src/aiagent/tasks.py` — `build_providers`,
`build_policy`, `build_critic`. This is the one place that picks live vs. fake
adapters; wire your new adapter here.

## 3. Choose your models

No code change, just env (`.env`, see `.env.example`):

- **LLM** (ADR-041): `AGENT_LLM_BACKEND=anthropic|ollama`, `AGENT_MODEL_ID=…`.
  Anthropic-hosted or a local Ollama model — the factory `make_chat_model`
  handles both. Add `AGENT_MODEL_FALLBACKS` (ADR-052) to survive a provider
  outage — e.g. end on a keyless local Ollama.
- **Search / other keys**: your adapter reads its own env var (mirror how
  `TAVILY_API_KEY` is used).
- **Pricing** for the spend cap (ADR-038/048): set `LLM_COST_*` to your model's
  rates (or `0` for a local model).

## 4. The backend + frontend follow the result shape

The Rust backend is mostly domain-agnostic (jobs, auth, quotas, callbacks). The
one coupling is the **result shape**, pinned on all three sides (ADR-049):

1. `backend/src/domain/search_result.rs` — the `SearchResult` struct the backend
   stores and re-serves.
2. `contracts/*.json` — the golden fixtures asserted on both languages + the
   frontend. **Update these when you change the shape** — a drift breaks a test
   instead of production.
3. `frontend/src/api.ts` (zod schemas) and the Vue views/components that render
   a result (`src/views/`, `src/components/`).

Change the shape in these together; the contract tests (`backend/tests/contract.rs`,
`agent/tests/test_contract.py`, `frontend/src/__tests__/contract.spec.ts`) tell
you if you missed a side.

## 5. Keep the guardrails (don't rip these out)

These are boring and load-bearing — they're most of why the boilerplate exists:

- **Idempotency** (ADR-016): the job state machine + replace/upsert make Celery
  retries safe.
- **Spend cap** (ADR-048) and **step budget** (ADR-030): bound a runaway agent.
- **The callback contract** (ADR-006): the worker never touches the DB.
- **Defensive parsing** (ADR-043): a malformed LLM reply degrades to *finish*,
  never a crash.
- **Observability** (ADR-029/050) and **contract fixtures** (ADR-049).

## 6. Strip what you don't need

Each of these is optional; disable if your task has no use for it:

- **Recurring searches** (ADR-033) — scheduler + `/api/recurring` + the digest.
- **Human-in-the-loop** (ADR-032) — pass no `ClarificationRequester`; the policy
  can never `ask`.
- **Self-critique** (ADR-031) — pass no `ResultCritic`; the loop skips the review.
- **Digest webhooks** (ADR-036/047) — remove the `DigestSender` wiring.
- **The workflow mode** (ADR-030) — if you only want the agent loop.

## 7. Rename (cosmetic, optional)

- Agent Python package: `agent/pyproject.toml` `name`, the `agent/src/aiagent/`
  directory, and the `aiagent.` imports (a scoped find-and-replace).
- The Rust crate (`backend`) and the frontend package (`frontend`) are generic
  names — usually fine to leave.
- Docker image names in `docker-compose.yml`, and the repo/URLs.

## Suggested order

1. Get it running keyless first (`AGENT_PROVIDERS=fake`) — [COMMANDS.md](COMMANDS.md).
2. Reshape `models.py` + `ports.py` for your task (red tests are your checklist).
3. Rewrite the prompts + reply schemas in `llm.py`.
4. Write your `SearchProvider` adapter (+ a keyless fake); wire it in `tasks.py`.
5. Update the result shape across the three sides + the contract fixtures.
6. Adapt the frontend views; set your env/keys; run the eval harness (ADR-045)
   on a few golden cases before shipping.

Everything else — auth, the queue, retries, HITL, SSE, the spend cap, metrics,
the VPS deploy — you keep for free.
