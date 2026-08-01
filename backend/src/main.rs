//! Thin wiring binary: environment parsing/validation lives in
//! `backend::config::AppConfig` (tested), the HTTP surface in
//! `backend::adapters::http` (tested). This file only connects them —
//! it is excluded from coverage along with `src/bin/` (ADR-023).

use std::sync::Arc;

use backend::adapters::auth::{Argon2PasswordHasher, JwtTokenService};
use backend::adapters::digest::WebhookDigestSender;
use backend::adapters::dispatch::{HttpJobDispatcher, NoopJobDispatcher};
use backend::adapters::http::rate_limit::Limiter;
use backend::adapters::http::{router_with_limits, AppState};
use backend::adapters::leader_lock::{LeaderLock, NoopLeaderLock};
use backend::adapters::persistence::in_memory::{
    InMemoryJobRepository, InMemoryRecurringSearchRepository, InMemoryRefreshTokenRepository,
    InMemorySecurityAudit, InMemoryUserRepository,
};
use backend::adapters::persistence::postgres::{
    run_migrations, PostgresJobRepository, PostgresLeaderLock, PostgresRecurringSearchRepository,
    PostgresRefreshTokenRepository, PostgresSecurityAudit, PostgresUserRepository,
};
use backend::application::{FailStaleJobs, RunDueSearches};
use backend::config::AppConfig;
use backend::domain::ports::{
    JobDispatcher, JobRepository, RecurringSearchRepository, RefreshTokenRepository, SecurityAudit,
    UserRepository,
};
use sqlx::postgres::PgPoolOptions;

fn main() {
    // Loads ../.env (repo root) in development; harmless in containers.
    dotenvy::dotenv().ok();

    // Structured logs (ADR-018): LOG_FORMAT=json in production, pretty in dev.
    // OpenTelemetry traces (ADR-029) are opt-in: the OTLP layer only exists
    // when OTEL_EXPORTER_OTLP_ENDPOINT is set. The provider must be built
    // outside the tokio runtime (its blocking export client would shut the
    // batch processor down otherwise), hence the sync main.
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let (otel_provider, otel_layer) = match backend::telemetry::layer() {
        Some((provider, layer)) => (Some(provider), Some(layer)),
        None => (None, None),
    };
    // Metrics provider (ADR-050): same gate, global, flushed on exit.
    let meter_provider = backend::telemetry::meter_provider();
    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(otel_layer);
    match std::env::var("LOG_FORMAT").as_deref() {
        Ok("json") => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_current_span(true),
            )
            .init(),
        _ => registry.with(tracing_subscriber::fmt::layer()).init(),
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the tokio runtime")
        .block_on(serve());

    // Flush buffered spans and metrics before exiting (ADR-029/050).
    if let Some(provider) = otel_provider {
        if let Err(e) = provider.shutdown() {
            eprintln!("failed to flush OpenTelemetry spans: {e}");
        }
    }
    if let Some(provider) = meter_provider {
        if let Err(e) = provider.shutdown() {
            eprintln!("failed to flush OpenTelemetry metrics: {e}");
        }
    }
    tracing::info!("backend stopped gracefully");
}

async fn serve() {
    // Parse + validate the environment (ADR-020: fail fast in production).
    let config = match AppConfig::from_env() {
        Ok(config) => config,
        Err(missing) => {
            tracing::error!(
                missing = missing.join(", "),
                "backend cannot start in production: required environment \
                 variable(s) missing, empty, or left at a development \
                 placeholder (see .env.example)"
            );
            std::process::exit(1);
        }
    };
    for warning in &config.warnings {
        tracing::warn!("{warning}");
    }

    let dispatcher: Arc<dyn JobDispatcher> = match &config.agent_api_url {
        Some(url) => Arc::new(HttpJobDispatcher::new(
            url.clone(),
            config.internal_token.clone(),
        )),
        None => Arc::new(NoopJobDispatcher),
    };

    type Repos = (
        Arc<dyn UserRepository>,
        Arc<dyn JobRepository>,
        Arc<dyn RefreshTokenRepository>,
        Arc<dyn RecurringSearchRepository>,
        // Security audit log (ADR-057).
        Arc<dyn SecurityAudit>,
        // Single-leader gate for the background loop (ADR-053).
        Arc<dyn LeaderLock>,
    );
    let (users, jobs, refresh_tokens, recurring, audit, leader): Repos = match &config.database_url
    {
        Some(url) => {
            let pool = PgPoolOptions::new()
                .max_connections(10)
                .connect(url)
                .await
                .expect("failed to connect to PostgreSQL");
            run_migrations(&pool)
                .await
                .expect("failed to run migrations");
            tracing::info!("using PostgreSQL persistence");
            (
                Arc::new(PostgresUserRepository::new(pool.clone())),
                Arc::new(PostgresJobRepository::new(pool.clone())),
                Arc::new(PostgresRefreshTokenRepository::new(pool.clone())),
                Arc::new(PostgresRecurringSearchRepository::new(pool.clone())),
                Arc::new(PostgresSecurityAudit::new(pool.clone())),
                Arc::new(PostgresLeaderLock::new(pool)),
            )
        }
        None => (
            Arc::new(InMemoryUserRepository::default()),
            Arc::new(InMemoryJobRepository::default()),
            Arc::new(InMemoryRefreshTokenRepository::default()),
            Arc::new(InMemoryRecurringSearchRepository::default()),
            Arc::new(InMemorySecurityAudit::default()),
            // A single in-memory instance always leads.
            Arc::new(NoopLeaderLock),
        ),
    };

    // Background loop: the reaper (ADR-016), refresh-token purge (ADR-008)
    // and the recurring-search scheduler (ADR-033) share one ticker.
    let reaper = FailStaleJobs::new(
        jobs.clone(),
        std::time::Duration::from_secs(config.job_timeout_minutes * 60),
    );
    let scheduler = RunDueSearches::new(
        recurring.clone(),
        jobs.clone(),
        dispatcher.clone(),
        config.daily_search_quota,
    );
    let refresh_tokens_for_reaper = refresh_tokens.clone();
    let audit_for_purge = audit.clone();
    // Security-event retention (ADR-057): 0 keeps events forever (no purge).
    let security_retention_days = config.security_event_retention_days;
    let tick = std::time::Duration::from_secs(config.scheduler_tick_seconds);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(tick);
        loop {
            ticker.tick().await;
            // Only the leader runs the loop (ADR-053): with several replicas the
            // scheduler would otherwise launch duplicate recurring jobs.
            if !leader.acquire().await {
                continue;
            }
            match reaper.execute().await {
                Ok(0) => {}
                Ok(n) => tracing::warn!(reaped = n, "failed stale jobs (timeout)"),
                Err(e) => tracing::error!(error = %e, "job reaper run failed"),
            }
            match scheduler.execute().await {
                Ok(0) => {}
                Ok(n) => tracing::info!(launched = n, "recurring searches launched"),
                Err(e) => tracing::error!(error = %e, "recurring scheduler run failed"),
            }
            if let Err(e) = refresh_tokens_for_reaper
                .delete_expired(chrono::Utc::now())
                .await
            {
                tracing::error!(error = %e, "refresh token purge failed");
            }
            if security_retention_days > 0 {
                let cutoff = chrono::Utc::now() - chrono::Duration::days(security_retention_days);
                if let Err(e) = audit_for_purge.delete_before(cutoff).await {
                    tracing::error!(error = %e, "security event purge failed");
                }
            }
            leader.release().await;
        }
    });

    // Per-account login throttle (ADR-057): shares RATE_LIMIT_REDIS_URL so it is
    // distributed when the backend scales out (ADR-037), in-memory otherwise.
    let login_throttle = Limiter::per_minute(
        config.rate_limits.login_per_minute,
        "login",
        config.rate_limits.redis_url.as_deref(),
    );

    let state = AppState::new(
        users,
        jobs,
        refresh_tokens,
        recurring,
        dispatcher,
        Arc::new(WebhookDigestSender::new(
            config.digest_signing_secret,
            config.digest_allow_private_webhooks,
        )),
        Arc::new(Argon2PasswordHasher),
        Arc::new(JwtTokenService::new(&config.jwt_secret, 15)),
        audit,
        login_throttle,
        config.internal_token,
        config.daily_search_quota,
        config.refresh_token_days,
    );

    let listener = tokio::net::TcpListener::bind(&config.bind_addr)
        .await
        .expect("failed to bind");
    tracing::info!(bind_addr = %config.bind_addr, "backend listening");
    // Graceful shutdown (ADR-024): stop accepting connections on SIGTERM/SIGINT
    // (docker stop, VPS redeploy, ctrl-c) and drain in-flight requests —
    // including worker callbacks writing results — instead of dropping them.
    axum::serve(listener, router_with_limits(state, config.rate_limits))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install ctrl-c handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("SIGINT received, draining in-flight requests"),
        () = terminate => tracing::info!("SIGTERM received, draining in-flight requests"),
    }
}
