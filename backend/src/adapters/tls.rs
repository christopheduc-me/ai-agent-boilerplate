//! Process-wide rustls crypto provider (ADR-064).
//!
//! `reqwest` 0.13 no longer picks a crypto provider on its own: the `rustls`
//! feature would pull in **aws-lc-rs**, whose `aws-lc-sys` build script needs a
//! C toolchain (cmake + clang) in the builder image. The backend instead uses
//! `rustls-no-provider` and installs **ring** — the provider `lettre` already
//! brings in — so the dependency tree keeps a single provider and the Docker
//! build stays on plain `rust:slim`.
//!
//! With `rustls-no-provider`, building a `reqwest::Client` panics unless a
//! process-level provider is installed first, so every client constructor calls
//! [`ensure_crypto_provider`]. Doing it here rather than in `main` keeps unit
//! tests working too — they build clients without going through startup.

use std::sync::Once;

static INSTALL: Once = Once::new();

/// Installs the ring `CryptoProvider` once per process. Idempotent and safe to
/// call from any thread; a provider installed by someone else is left alone.
pub fn ensure_crypto_provider() {
    INSTALL.call_once(|| {
        // Errors mean a provider is already installed — nothing to do.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
