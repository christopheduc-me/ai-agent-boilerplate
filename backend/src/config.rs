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
}
