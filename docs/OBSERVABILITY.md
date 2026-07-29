# Observability & what to measure

How to see what the system is doing, and the handful of signals worth watching
when you operate a fork. Everything here is **opt-in** and standards-based
(OpenTelemetry), so you can point it at any backend — Jaeger is only the
dev-time default (see [ARCHITECTURE.md](ARCHITECTURE.md) ADR-018/029/038/045/048).

## The pillars, and their status

| Pillar | Status | Where |
|---|---|---|
| **Traces** | ✅ opt-in (ADR-029) | Jaeger UI (`:16686`), per-search trace across all four processes, with a span per LLM call |
| **Logs** | ✅ structured (ADR-018) | stdout, one JSON object per line when `LOG_FORMAT=json`, with correlation ids |
| **Metrics** | ⬜ deferred | not shipped — the third pillar, a documented next step (ARCHITECTURE §5) |

## Turning tracing on

```sh
docker compose --profile observability up -d       # adds Jaeger (OTLP :4318, UI :16686)
# the app services pass OTEL_EXPORTER_OTLP_ENDPOINT through from the environment
```

Unset the variable and nothing is installed — the processes behave exactly as
before, at zero cost. A production fork points the variable at its own collector
(Grafana Tempo, Honeycomb, Datadog…); **no code changes** — that is the whole
point of emitting via OTLP.

## Correlating logs and traces

Every log line carries the correlation keys (`request_id`, `job_id`, ADR-018)
and — when tracing is on — the active **`trace_id`** / **`span_id`** (ADR-029
amendment). So the two directions both work:

- **log → trace**: copy a line's `trace_id`, paste it into Jaeger's search.
- **trace → logs**: take the `trace_id` from Jaeger, `grep` it across stdout.

The `job_id` remains the cross-service key that ties a whole research run
together even with tracing off.

## The signals that matter here

Ordered by how often they earn their keep. Most live in the span attributes and
the job record already; a few would become proper metrics if/when the third
pillar lands.

| Signal | Why you watch it | Where to find it today |
|---|---|---|
| **Cost per job** (`cost_usd`) | the spend you actually pay | job detail (`GET /api/searches/{id}` → `usage`), `usage-callback`, logs |
| **Spend-cap trips** (ADR-048) | a runaway / mis-priced model | the run's final journal step: reason `cost budget of $… exhausted` |
| **Job success vs failure** | baseline health | job `status` (`completed` / `failed`); the reaper marks timeouts failed |
| **Agent action mix** (search/ask/finish) | is the agent behaving? | span attribute `aiagent.agent.action` on each `llm decide` |
| **Critique drops & gaps** (ADR-031) | result quality / noise | span attributes `aiagent.critic.dropped`, `aiagent.critic.has_gap` |
| **HITL pause rate** (ADR-032) | how ambiguous the goals are | job status `awaiting_input`; the `question` callback |
| **LLM tokens & latency** | the real cost/perf drivers | `gen_ai.usage.*` attributes + span duration on each `llm …` span |
| **Quota rejections** (ADR-017) | abuse or a too-tight limit | `429` responses / rate-limit log lines |
| **Per-run quality score** (ADR-045) | prompt/model regressions | the evaluation harness — run on demand, see COMMANDS.md |

## Reading a run in Jaeger

One search = one trace. Under the request/worker spans you will see the agent's
decisions in order: each `llm decide` (with its chosen action), the searches,
the `llm critique`, and the batched `llm enrich` — every one tagged with its
model and token usage. A slow or expensive run shows *which* call caused it, and
a failed call is already a red span (the exception is recorded automatically).

## Best practices for a fork

- **Keep it opt-in.** Don't hardwire a collector; drive it from
  `OTEL_EXPORTER_OTLP_ENDPOINT` so the keyless demo and CI stay clean.
- **Sample in production.** Tracing 100 % of traffic is fine in dev; add
  ratio-based sampling in production. The Python SDK honours
  `OTEL_TRACES_SAMPLER=parentbased_traceidratio` out of the box; the Rust
  provider's sampler is set in `backend/src/telemetry.rs` (default: always-on).
- **Alert on the signals above, not on log volume.** Cost-per-day and
  failure-rate are the two that catch real incidents first — those are the first
  metrics to add when you stand up the third pillar.
- **Never trace secrets.** Prompts and results can carry user data; the spans
  here record *metadata* (model, tokens, decision), never prompt/response text —
  keep it that way.
