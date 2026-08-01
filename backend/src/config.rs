//! Fail-fast startup checks (ADR-020).
//!
//! In development every variable has a graceful fallback (in-memory storage,
//! noop dispatcher, insecure dev secret + warning). In production
//! (`APP_ENV=production`) the same gaps must abort startup with an explicit
//! message instead of running degraded or failing on the first request.

/// Values that must never survive into production.
const PLACEHOLDERS: &[&str] = &["change-me", "insecure-dev-secret"];

/// Returns the variables (among `names`) that are missing, empty, or left at a
/// development placeholder. `lookup` abstracts `std::env::var` for testability.
pub fn missing_required<F>(names: &[&str], lookup: F) -> Vec<String>
where
    F: Fn(&str) -> Option<String>,
{
    names
        .iter()
        .filter(|name| {
            !lookup(name).is_some_and(|v| !v.is_empty() && !PLACEHOLDERS.contains(&v.as_str()))
        })
        .map(|name| name.to_string())
        .collect()
}

/// Variables that must be set (non-empty, non-placeholder) in production.
pub const REQUIRED_IN_PRODUCTION: &[&str] = &[
    "JWT_SECRET",
    "INTERNAL_API_TOKEN",
    "DATABASE_URL",
    "AGENT_API_URL",
];

use crate::adapters::http::RateLimitConfig;

/// Everything `main` reads from the environment, parsed and validated in one
/// place so the wiring binary stays a thin shell (and this logic is testable).
#[derive(Debug)]
pub struct AppConfig {
    pub jwt_secret: String,
    pub internal_token: String,
    pub agent_api_url: Option<String>,
    pub database_url: Option<String>,
    pub job_timeout_minutes: u64,
    /// Cadence of the background loop (reaper + recurring scheduler, ADR-033).
    pub scheduler_tick_seconds: u64,
    pub daily_search_quota: u32,
    pub rate_limits: RateLimitConfig,
    pub refresh_token_days: i64,
    pub bind_addr: String,
    /// Shared secret for signing outbound digest webhooks (ADR-047). None
    /// (unset/empty) leaves digests unsigned — opt-in, like the Redis limiter.
    pub digest_signing_secret: Option<String>,
    /// Allow digest webhooks to target private/internal addresses (ADR-055):
    /// off by default (SSRF guard on the user-supplied URL), opt-in for a fork
    /// whose notification service (n8n, a relay…) lives on the same trusted
    /// private network.
    pub digest_allow_private_webhooks: bool,
    /// Retention for the security audit log (ADR-057): events older than this
    /// are purged by the background loop. Kept generous so an incident stays
    /// investigable; 0 disables the purge (keep forever).
    pub security_event_retention_days: i64,
    /// Degraded-mode notices to log once tracing is up (dev fallbacks, ADR-013).
    pub warnings: Vec<&'static str>,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, Vec<String>> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Errors with the list of missing variables when `APP_ENV=production`
    /// requirements are not met (ADR-020); otherwise applies the documented
    /// development fallbacks and records a warning for each one used.
    pub fn from_lookup<F>(lookup: F) -> Result<Self, Vec<String>>
    where
        F: Fn(&str) -> Option<String>,
    {
        if lookup("APP_ENV").as_deref() == Some("production") {
            let missing = missing_required(REQUIRED_IN_PRODUCTION, &lookup);
            if !missing.is_empty() {
                return Err(missing);
            }
        }

        let get = |name: &str| lookup(name).filter(|v| !v.is_empty());
        let get_u32 =
            |name: &str, default: u32| get(name).and_then(|v| v.parse().ok()).unwrap_or(default);
        let mut warnings = Vec::new();

        let jwt_secret = get("JWT_SECRET").unwrap_or_else(|| {
            warnings.push("JWT_SECRET not set, using an insecure development default");
            "insecure-dev-secret".into()
        });
        let agent_api_url = get("AGENT_API_URL");
        if agent_api_url.is_none() {
            warnings.push("AGENT_API_URL not set, jobs will not be dispatched (noop)");
        }
        let database_url = get("DATABASE_URL");
        if database_url.is_none() {
            warnings
                .push("DATABASE_URL not set, using in-memory persistence (data lost on restart)");
        }

        let internal_token = get("INTERNAL_API_TOKEN").unwrap_or_else(|| "change-me".into());
        // Weak-secret warning (ADR-055): a short HMAC/JWT key is brute-forceable.
        // A warning, not a hard fail, so it never breaks an existing deployment.
        const MIN_SECRET_LEN: usize = 32;
        let too_short = |v: &str| v.len() < MIN_SECRET_LEN && !PLACEHOLDERS.contains(&v);
        if too_short(&jwt_secret) {
            warnings.push("JWT_SECRET is shorter than 32 characters — use a long, random value");
        }
        if too_short(&internal_token) {
            warnings.push(
                "INTERNAL_API_TOKEN is shorter than 32 characters — use a long, random value",
            );
        }

        Ok(Self {
            jwt_secret,
            internal_token,
            agent_api_url,
            database_url,
            job_timeout_minutes: u64::from(get_u32("JOB_TIMEOUT_MINUTES", 15)),
            scheduler_tick_seconds: u64::from(get_u32("SCHEDULER_TICK_SECONDS", 60)).max(1),
            daily_search_quota: get_u32("DAILY_SEARCH_QUOTA", 20),
            rate_limits: RateLimitConfig {
                auth_per_minute: get_u32("RATE_LIMIT_AUTH_PER_MINUTE", 10),
                api_per_minute: get_u32("RATE_LIMIT_API_PER_MINUTE", 120),
                // Per-account login throttle (ADR-057), email-keyed.
                login_per_minute: get_u32("LOGIN_MAX_ATTEMPTS_PER_MINUTE", 10),
                // Distributed limiting (ADR-037): only when scaling out.
                redis_url: get("RATE_LIMIT_REDIS_URL"),
            },
            refresh_token_days: i64::from(get_u32("REFRESH_TOKEN_DAYS", 30)),
            bind_addr: get("BIND_ADDR").unwrap_or_else(|| "0.0.0.0:8000".into()),
            digest_signing_secret: get("DIGEST_SIGNING_SECRET"),
            digest_allow_private_webhooks: get("DIGEST_ALLOW_PRIVATE_WEBHOOKS")
                .is_some_and(|v| v == "true"),
            security_event_retention_days: i64::from(get_u32("SECURITY_EVENT_RETENTION_DAYS", 90)),
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_from(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<&'static str, &'static str> = pairs.iter().copied().collect();
        move |name| map.get(name).map(|v| v.to_string())
    }

    #[test]
    fn all_present_and_real_means_nothing_missing() {
        let lookup = lookup_from(&[("JWT_SECRET", "s3cret"), ("DATABASE_URL", "postgres://x")]);
        assert!(missing_required(&["JWT_SECRET", "DATABASE_URL"], lookup).is_empty());
    }

    #[test]
    fn short_secrets_warn_and_strong_ones_do_not() {
        // ADR-055: a real-but-short secret is flagged (not failed).
        let weak = AppConfig::from_lookup(lookup_from(&[("JWT_SECRET", "short")])).unwrap();
        assert!(weak
            .warnings
            .iter()
            .any(|w| w.contains("JWT_SECRET is shorter")));

        let strong = AppConfig::from_lookup(|name| match name {
            "JWT_SECRET" => Some("x".repeat(40)),
            "INTERNAL_API_TOKEN" => Some("y".repeat(40)),
            _ => None,
        })
        .unwrap();
        assert!(!strong.warnings.iter().any(|w| w.contains("shorter than")));
    }

    #[test]
    fn digest_private_webhooks_are_opt_in() {
        // ADR-055: off by default (SSRF guard), on only when explicitly set.
        assert!(
            !AppConfig::from_lookup(lookup_from(&[]))
                .unwrap()
                .digest_allow_private_webhooks
        );
        assert!(
            AppConfig::from_lookup(lookup_from(&[("DIGEST_ALLOW_PRIVATE_WEBHOOKS", "true")]))
                .unwrap()
                .digest_allow_private_webhooks
        );
    }

    #[test]
    fn unset_and_empty_variables_are_missing() {
        let lookup = lookup_from(&[("EMPTY", "")]);
        assert_eq!(
            missing_required(&["EMPTY", "UNSET"], lookup),
            vec!["EMPTY".to_string(), "UNSET".to_string()]
        );
    }

    #[test]
    fn development_placeholders_count_as_missing() {
        let lookup = lookup_from(&[
            ("INTERNAL_API_TOKEN", "change-me"),
            ("JWT_SECRET", "insecure-dev-secret"),
        ]);
        assert_eq!(
            missing_required(&["INTERNAL_API_TOKEN", "JWT_SECRET"], lookup),
            vec!["INTERNAL_API_TOKEN".to_string(), "JWT_SECRET".to_string()]
        );
    }

    #[test]
    fn app_config_applies_development_fallbacks_with_warnings() {
        let config = AppConfig::from_lookup(lookup_from(&[])).unwrap();

        assert_eq!(config.jwt_secret, "insecure-dev-secret");
        assert_eq!(config.internal_token, "change-me");
        assert_eq!(config.agent_api_url, None);
        assert_eq!(config.database_url, None);
        assert_eq!(config.job_timeout_minutes, 15);
        assert_eq!(config.daily_search_quota, 20);
        assert_eq!(config.rate_limits.auth_per_minute, 10);
        assert_eq!(config.rate_limits.api_per_minute, 120);
        assert_eq!(config.refresh_token_days, 30);
        assert_eq!(config.bind_addr, "0.0.0.0:8000");
        assert_eq!(config.warnings.len(), 3); // jwt, agent url, database
    }

    #[test]
    fn app_config_parses_overrides_and_ignores_garbage_numbers() {
        let config = AppConfig::from_lookup(lookup_from(&[
            ("JWT_SECRET", "the-quick-brown-fox-jumps-over-the-lazy-dog"),
            ("AGENT_API_URL", "http://agent:8001"),
            ("DATABASE_URL", "postgres://x"),
            ("DAILY_SEARCH_QUOTA", "5"),
            ("RATE_LIMIT_AUTH_PER_MINUTE", "not-a-number"),
            ("BIND_ADDR", "127.0.0.1:9000"),
        ]))
        .unwrap();

        assert_eq!(config.daily_search_quota, 5);
        assert_eq!(config.rate_limits.auth_per_minute, 10); // fallback on parse error
        assert_eq!(config.bind_addr, "127.0.0.1:9000");
        assert!(config.warnings.is_empty());
    }

    #[test]
    fn app_config_rejects_incomplete_production_env() {
        let err = AppConfig::from_lookup(lookup_from(&[
            ("APP_ENV", "production"),
            ("JWT_SECRET", "real-secret"),
            ("INTERNAL_API_TOKEN", "change-me"), // placeholder -> missing
        ]))
        .unwrap_err();

        assert_eq!(
            err,
            vec!["INTERNAL_API_TOKEN", "DATABASE_URL", "AGENT_API_URL"]
        );
    }

    #[test]
    fn app_config_accepts_a_complete_production_env() {
        let config = AppConfig::from_lookup(lookup_from(&[
            ("APP_ENV", "production"),
            ("JWT_SECRET", "the-quick-brown-fox-jumps-over-the-lazy-dog"),
            (
                "INTERNAL_API_TOKEN",
                "pack-my-box-with-five-dozen-liquor-jugs-ok",
            ),
            ("DATABASE_URL", "postgres://x"),
            ("AGENT_API_URL", "http://agent:8001"),
        ]))
        .unwrap();
        assert!(config.warnings.is_empty());
    }
}
