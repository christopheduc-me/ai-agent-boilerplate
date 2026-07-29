# Observability & what to measure

How to see what the system is doing, and the handful of signals worth watching
when you operate a fork. Everything here is **opt-in** and standards-based
(OpenTelemetry), so you can point it at any backend — Jaeger is only the
dev-time default (see [ARCHITECTURE.md](ARCHITECTURE.md) ADR-018/029/038/045/048).

## The pillars, and their status

| Pillar | Status | Where |
|---|---|---|
| **Traces** | ✅ opt-in (ADR-029) | Jaeger UI (`:16686`), per-search trace across all four processes, with a span per LLM call |
| **Logs** | ✅ structured (ADR-018) | stdout, one JSON object per line when `LOG_FORMAT=json`, with correlation ids + `trace_id` |
| **Metrics** | ✅ opt-in (ADR-050) | Prometheus (`:9090`) + Grafana (`:3001`); LLM latency/tokens/cost and HTTP RED |

## Turning observability on

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4318 \
  docker compose --profile full --profile observability up -d
```

The **observability profile** adds an OpenTelemetry Collector (the OTLP entry
point), Jaeger, Prometheus and Grafana. The collector fans the signals out —
traces → Jaeger, metrics → Prometheus — so one endpoint feeds both pillars:

| UI | URL |
|---|---|
| Jaeger (traces) | http://localhost:16686 |
| Prometheus (metrics) | http://localhost:9090 |
| Grafana (dashboards) | http://localhost:3001 |

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

## Metrics & starter PromQL (ADR-050)

The agent and backend push OTel metrics through the collector to Prometheus.
Grafana ships with the Prometheus datasource **and a starter "AI agent overview"
dashboard** provisioned at startup (http://localhost:3001) — extend it with
these queries:

| Signal | PromQL |
|---|---|
| LLM call latency (p95, by op) | `histogram_quantile(0.95, sum by (le, operation) (rate(aiagent_llm_call_duration_seconds_bucket[5m])))` |
| Token throughput (by type) | `sum by (type) (rate(aiagent_llm_tokens_total[5m]))` |
| Spend rate ($/min, by outcome) | `sum by (outcome) (rate(aiagent_job_cost_USD_total[5m])) * 60` |
| Job outcomes | `sum by (outcome) (increase(aiagent_jobs_total[1h]))` |
| HTTP request rate (RED) | `sum by (route, status) (rate(http_server_requests_total[5m]))` |
| HTTP p95 latency (RED) | `histogram_quantile(0.95, sum by (le, route) (rate(http_server_duration_seconds_bucket[5m])))` |
| HTTP error ratio (RED) | `sum(rate(http_server_requests_total{status=~"5.."}[5m])) / sum(rate(http_server_requests_total[5m]))` |

> Metric names are the OTLP → Prometheus translation (dots → underscores, unit
> suffixes, `_total` on counters, `_bucket` on histograms). Confirm the exact
> names in the Prometheus UI (`:9090`) if a query returns nothing.

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
- **Alert on the signals above, not on log volume.** Spend rate and the HTTP
  error ratio are the two that catch real incidents first — wire Prometheus
  alerting rules on those before anything else.
- **Never trace secrets.** Prompts and results can carry user data; the spans
  here record *metadata* (model, tokens, decision), never prompt/response text —
  keep it that way.
