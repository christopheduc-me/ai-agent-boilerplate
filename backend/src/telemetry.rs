//! Opt-in OpenTelemetry traces (ADR-029).
//!
//! Everything is gated on `OTEL_EXPORTER_OTLP_ENDPOINT`: when the variable is
//! unset (the default), no exporter, no layer and no propagator are installed
//! and the process behaves exactly as before. When set (e.g. to a local Jaeger
//! via `docker compose --profile observability`), every HTTP request gets a
//! span (see `main.rs`), and the dispatcher propagates the W3C `traceparent`
//! header so the Python agent continues the same trace (FastAPI -> Celery ->
//! callbacks) — the distributed extension of the ADR-018 correlation ids.

use opentelemetry::global;
use opentelemetry::propagation::Injector;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing_opentelemetry::{OpenTelemetryLayer, OpenTelemetrySpanExt};

/// Builds the OTLP tracing layer when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
///
/// Returns the provider too: the caller keeps it alive and calls
/// `shutdown()` on exit to flush buffered spans. The OTLP exporter reads the
/// endpoint (and the other standard `OTEL_*` variables) from the environment.
#[allow(clippy::type_complexity)]
pub fn layer<S>() -> Option<(
    SdkTracerProvider,
    OpenTelemetryLayer<S, opentelemetry_sdk::trace::SdkTracer>,
)>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    // An empty value (e.g. a compose passthrough default) means disabled too.
    std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|endpoint| !endpoint.trim().is_empty())?;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()
        .expect("failed to build the OTLP span exporter");
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(Resource::builder().with_service_name("backend").build())
        .build();
    global::set_text_map_propagator(TraceContextPropagator::new());

    let layer = tracing_opentelemetry::layer().with_tracer(provider.tracer("backend"));
    Some((provider, layer))
}

/// Installs the OTLP **metric** provider (ADR-050) behind the same gate, so the
/// HTTP RED instruments (`adapters/http/request_id`) export via OTLP. Returns
/// the provider for the caller to `shutdown()` on exit (flush). `None` — and no
/// global provider — when telemetry is off, keeping the instruments no-ops.
pub fn meter_provider() -> Option<opentelemetry_sdk::metrics::SdkMeterProvider> {
    std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|endpoint| !endpoint.trim().is_empty())?;

    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .build()
        .expect("failed to build the OTLP metric exporter");
    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(Resource::builder().with_service_name("backend").build())
        .build();
    global::set_meter_provider(provider.clone());
    Some(provider)
}

struct HeaderInjector<'a>(&'a mut reqwest::header::HeaderMap);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::try_from(key),
            reqwest::header::HeaderValue::try_from(value),
        ) {
            self.0.insert(name, value);
        }
    }
}

/// Injects the current trace context (W3C `traceparent`) into outbound
/// headers. A no-op unless telemetry is enabled: without the OTLP layer the
/// current span carries no OpenTelemetry context and the default global
/// propagator injects nothing.
pub fn inject_trace_context(headers: &mut reqwest::header::HeaderMap) {
    let context = tracing::Span::current().context();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut HeaderInjector(headers));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn layer_is_disabled_without_the_endpoint_variable() {
        // The suite never sets OTEL_EXPORTER_OTLP_ENDPOINT (no collector in
        // unit tests), so the gate must return None.
        assert!(std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_err());
        assert!(layer::<tracing_subscriber::Registry>().is_none());
    }

    #[test]
    fn inject_is_a_noop_without_telemetry() {
        let mut headers = reqwest::header::HeaderMap::new();
        inject_trace_context(&mut headers);
        assert!(headers.is_empty());
    }

    #[test]
    fn inject_adds_traceparent_inside_an_instrumented_span() {
        // Local provider without exporter: spans are created (and sampled)
        // but exported nowhere — enough to exercise the propagation path.
        let provider = SdkTracerProvider::builder().build();
        global::set_text_map_propagator(TraceContextPropagator::new());
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request");
            let _guard = span.enter();
            let mut headers = reqwest::header::HeaderMap::new();
            inject_trace_context(&mut headers);
            let traceparent = headers
                .get("traceparent")
                .expect("traceparent header injected")
                .to_str()
                .unwrap();
            // W3C format: version-traceid-spanid-flags
            assert_eq!(traceparent.split('-').count(), 4);
        });
    }
}
