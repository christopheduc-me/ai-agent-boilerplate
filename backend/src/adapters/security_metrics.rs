//! Metering decorator over the `SecurityAudit` port (ADR-060, extends ADR-050).
//!
//! Wraps any `SecurityAudit` and emits a `security.events` counter (labelled by
//! `kind`) on every recorded event, then delegates. As a decorator over the
//! port it counts *every* call site — the HTTP handlers (login failed/throttled,
//! quota) and the refresh use case (reuse) — with no change to those callers,
//! and works with any backing store (in-memory or Postgres).
//!
//! The counter is a no-op until a `MeterProvider` is installed (telemetry off,
//! ADR-050), so it costs nothing in the keyless demo. `kind` is a closed set
//! (`SecurityEventKind`), so its cardinality is bounded.

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use opentelemetry::metrics::Counter;
use opentelemetry::{global, KeyValue};

use crate::domain::ports::{PortError, SecurityAudit};
use crate::domain::SecurityEvent;

static EVENTS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    global::meter("backend")
        .u64_counter("security.events")
        .with_description("Security audit events, by kind (ADR-057)")
        .build()
});

pub struct MeteredSecurityAudit {
    inner: Arc<dyn SecurityAudit>,
}

impl MeteredSecurityAudit {
    pub fn new(inner: Arc<dyn SecurityAudit>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl SecurityAudit for MeteredSecurityAudit {
    async fn record(&self, event: &SecurityEvent) -> Result<(), PortError> {
        EVENTS.add(1, &[KeyValue::new("kind", event.kind.clone())]);
        self.inner.record(event).await
    }

    async fn list_recent(&self, limit: i64) -> Result<Vec<SecurityEvent>, PortError> {
        self.inner.list_recent(limit).await
    }

    async fn delete_before(&self, cutoff: DateTime<Utc>) -> Result<u64, PortError> {
        self.inner.delete_before(cutoff).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::InMemorySecurityAudit;
    use crate::domain::SecurityEventKind;

    #[tokio::test]
    async fn records_through_to_the_wrapped_audit_and_counts() {
        // The counter is a no-op without a MeterProvider; assert the decorator
        // preserves the port's behavior (delegation) — the metric is a side effect.
        let inner = Arc::new(InMemorySecurityAudit::default());
        let metered = MeteredSecurityAudit::new(inner.clone());

        let event = SecurityEvent::new(SecurityEventKind::LoginFailed, None, None, "a@b.com");
        metered.record(&event).await.unwrap();

        let listed = metered.list_recent(10).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, "login_failed");

        assert_eq!(metered.delete_before(Utc::now()).await.unwrap(), 1);
        assert!(inner.list_recent(10).await.unwrap().is_empty());
    }
}
