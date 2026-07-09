use std::sync::Arc;

use backend::adapters::auth::{Argon2PasswordHasher, JwtTokenService};
use backend::adapters::dispatch::{HttpJobDispatcher, NoopJobDispatcher};
use backend::adapters::http::{router_with_limits, AppState, RateLimitConfig};
use backend::adapters::persistence::in_memory::{
    InMemoryJobRepository, InMemoryRefreshTokenRepository, InMemoryUserRepository,
};
use backend::adapters::persistence::postgres::{
    run_migrations, PostgresJobRepository, PostgresRefreshTokenRepository, PostgresUserRepository,
};
use backend::application::FailStaleJobs;
use backend::domain::ports::{
    JobDispatcher, JobRepository, RefreshTokenRepository, UserRepository,
};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() {
    // Loads ../.env (repo root) in development; harmless in containers.
    dotenvy::dotenv().ok();

    // Structured logs (ADR-018): LOG_FORMAT=json in production, pretty in dev.
    let env_filter =
        || tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    match std::env::var("LOG_FORMAT").as_deref() {
        Ok("json") => tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_env_filter(env_filter())
            .init(),
        _ => tracing_subscriber::fmt()
            .with_env_filter(env_filter())
            .init(),
    }

    // Fail-fast startup check (ADR-020): in production, every required
    // variable must be set — no degraded fallbacks, no placeholder secrets.
    if std::env::var("APP_ENV").as_deref() == Ok("production") {
        let missing =
            backend::config::missing_required(backend::config::REQUIRED_IN_PRODUCTION, |name| {
                std::env::var(name).ok()
            });
        if !missing.is_empty() {
            tracing::error!(
                missing = missing.join(", "),
                "backend cannot start in production: required environment \
                 variable(s) missing, empty, or left at a development \
                 placeholder (see .env.example)"
            );
            std::process::exit(1);
        }
    }

    let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| {
        tracing::warn!("JWT_SECRET not set, using an insecure development default");
        "insecure-dev-secret".into()
    });
    let internal_token = std::env::var("INTERNAL_API_TOKEN").unwrap_or_else(|_| "change-me".into());

    let dispatcher: Arc<dyn JobDispatcher> = match std::env::var("AGENT_API_URL") {
        Ok(url) => Arc::new(HttpJobDispatcher::new(url, internal_token.clone())),
        Err(_) => {
            tracing::warn!("AGENT_API_URL not set, jobs will not be dispatched (noop)");
            Arc::new(NoopJobDispatcher)
        }
    };

    type Repos = (
        Arc<dyn UserRepository>,
        Arc<dyn JobRepository>,
        Arc<dyn RefreshTokenRepository>,
    );
    let (users, jobs, refresh_tokens): Repos = match std::env::var("DATABASE_URL") {
        Ok(url) => {
            let pool = PgPoolOptions::new()
                .max_connections(10)
                .connect(&url)
                .await
                .expect("failed to connect to PostgreSQL");
            run_migrations(&pool)
                .await
                .expect("failed to run migrations");
            tracing::info!("using PostgreSQL persistence");
            (
                Arc::new(PostgresUserRepository::new(pool.clone())),
                Arc::new(PostgresJobRepository::new(pool.clone())),
                Arc::new(PostgresRefreshTokenRepository::new(pool)),
            )
        }
        Err(_) => {
            tracing::warn!(
                "DATABASE_URL not set, using in-memory persistence (data lost on restart)"
            );
            (
                Arc::new(InMemoryUserRepository::default()),
                Arc::new(InMemoryJobRepository::default()),
                Arc::new(InMemoryRefreshTokenRepository::default()),
            )
        }
    };

    // Background reaper (ADR-016): fails jobs stuck without worker notification
    // and purges expired refresh tokens (ADR-008).
    let timeout_minutes: u64 = std::env::var("JOB_TIMEOUT_MINUTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let reaper = FailStaleJobs::new(
        jobs.clone(),
        std::time::Duration::from_secs(timeout_minutes * 60),
    );
    let refresh_tokens_for_reaper = refresh_tokens.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            ticker.tick().await;
            match reaper.execute().await {
                Ok(0) => {}
                Ok(n) => tracing::warn!(reaped = n, "failed stale jobs (timeout)"),
                Err(e) => tracing::error!(error = %e, "job reaper run failed"),
            }
            if let Err(e) = refresh_tokens_for_reaper
                .delete_expired(chrono::Utc::now())
                .await
            {
                tracing::error!(error = %e, "refresh token purge failed");
            }
        }
    });

    fn env_u32(name: &str, default: u32) -> u32 {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    // Abuse protection (ADR-017): per-user quota + per-IP rate limits.
    let daily_search_quota = env_u32("DAILY_SEARCH_QUOTA", 20);
    let limits = RateLimitConfig {
        auth_per_minute: env_u32("RATE_LIMIT_AUTH_PER_MINUTE", 10),
        api_per_minute: env_u32("RATE_LIMIT_API_PER_MINUTE", 120),
    };

    let refresh_ttl_days = i64::from(env_u32("REFRESH_TOKEN_DAYS", 30));

    let state = AppState::new(
        users,
        jobs,
        refresh_tokens,
        dispatcher,
        Arc::new(Argon2PasswordHasher),
        Arc::new(JwtTokenService::new(&jwt_secret, 15)),
        internal_token,
        daily_search_quota,
        refresh_ttl_days,
    );

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8000".into());
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("failed to bind");
    tracing::info!(%bind_addr, "backend listening");
    axum::serve(listener, router_with_limits(state, limits))
        .await
        .expect("server error");
}
