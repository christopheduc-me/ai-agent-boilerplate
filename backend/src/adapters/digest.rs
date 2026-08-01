//! DigestSender adapters (ADR-036).
//!
//! The webhook adapter POSTs the digest JSON to the URL saved on the
//! recurring search — the universal integration surface (Slack, n8n, Zapier,
//! a fork's own endpoint…). An e-mail sender is one more adapter behind the
//! same port. Deliveries are short-lived and best-effort: the caller treats
//! failures as log-and-continue.
//!
//! Digests are **at-least-once** (a redelivered completion re-sends the same
//! `job_id`, so consumers dedup on it). When a signing secret is configured
//! (ADR-047) each POST also carries an HMAC-SHA256 of the body in the
//! `X-Signature-256` header, so the consumer can *authenticate* it — a public
//! webhook URL is otherwise forgeable by anyone who learns it.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use sha2::Sha256;

use crate::domain::ports::{Digest, DigestSender, PortError};

type HmacSha256 = Hmac<Sha256>;

/// Header carrying `sha256=<hex HMAC of the raw body>` (GitHub convention).
const SIGNATURE_HEADER: &str = "X-Signature-256";

/// True if `ip` is not a public/global address — loopback, private, link-local,
/// CGNAT, etc. (ADR-055). Used to keep the user-supplied webhook URL from
/// reaching internal services (SSRF).
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                // 100.64.0.0/10 carrier-grade NAT.
                || (v4.octets()[0] == 100 && (0x40..=0x7f).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || matches!(v6.to_ipv4_mapped(), Some(v4) if is_blocked_ip(IpAddr::V4(v4)))
                // Unique local fc00::/7.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local fe80::/10.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Rejects a webhook URL whose host resolves to a non-public address (ADR-055),
/// so an authenticated user cannot point the digest at `169.254.169.254`,
/// `localhost`, `redis:6379`, the internal API… Combined with redirects being
/// disabled on the client, the request can only reach a public host.
async fn ensure_public_host(url: &str, allow_private: bool) -> Result<(), PortError> {
    if allow_private {
        return Ok(());
    }
    let parsed =
        reqwest::Url::parse(url).map_err(|e| PortError(format!("invalid webhook url: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| PortError("webhook url has no host".into()))?;
    let port = parsed.port_or_known_default().unwrap_or(0);
    let mut resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| PortError(format!("cannot resolve webhook host: {e}")))?
        .peekable();
    if resolved.peek().is_none() {
        return Err(PortError("webhook host did not resolve".into()));
    }
    for addr in resolved {
        if is_blocked_ip(addr.ip()) {
            return Err(PortError(format!(
                "webhook host resolves to a non-public address ({})",
                addr.ip()
            )));
        }
    }
    Ok(())
}

/// SSRF-safe DNS resolver (ADR-056). It resolves the host and refuses the
/// connection if *any* resolved address is non-public. Because reqwest connects
/// to exactly the addresses this returns, the validation and the connect share
/// one resolution — closing the DNS-rebinding (TOCTOU) window that a plain
/// resolve-then-check pre-flight leaves open (an attacker's DNS answering a
/// public IP to the check, then `127.0.0.1` to the connect). `ensure_public_host`
/// still runs first for a fast, clear rejection; this is the race-free backstop.
struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            // Port 0: we only care about the addresses, reqwest applies the real
            // port. Same GAI path hyper-util's default resolver would take.
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if let Some(addr) = addrs.iter().find(|a| is_blocked_ip(a.ip())) {
                return Err(format!(
                    "webhook host resolves to a non-public address ({})",
                    addr.ip()
                )
                .into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

pub struct WebhookDigestSender {
    client: reqwest::Client,
    secret: Option<String>,
    // Skip the SSRF host check (ADR-055): the DIGEST_ALLOW_PRIVATE_WEBHOOKS
    // opt-in, and tests hitting a local stub.
    allow_private: bool,
}

impl WebhookDigestSender {
    /// With a `secret`, every digest is signed with HMAC-SHA256 (ADR-047); an
    /// empty secret is treated as no secret (unsigned). `allow_private` skips the
    /// SSRF host check (ADR-055) — off in production unless a fork explicitly
    /// opts in via `DIGEST_ALLOW_PRIVATE_WEBHOOKS` for an internal notification
    /// target on a trusted network.
    pub fn new(secret: Option<String>, allow_private: bool) -> Self {
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            // No redirects (ADR-055): a public URL must not 3xx to an internal
            // one after the host check.
            .redirect(reqwest::redirect::Policy::none());
        // Race-free SSRF guard (ADR-056): filter at connect-time DNS resolution
        // so rebinding cannot slip an internal IP past the pre-flight check.
        // The opt-in (allow_private) keeps the default resolver for internal
        // targets on a trusted network — and for tests hitting a local stub.
        if !allow_private {
            builder = builder.dns_resolver(Arc::new(PublicOnlyResolver));
        }
        Self {
            client: builder.build().expect("reqwest client"),
            secret: secret.filter(|s| !s.is_empty()),
            allow_private,
        }
    }
}

impl Default for WebhookDigestSender {
    fn default() -> Self {
        Self::new(None, false)
    }
}

/// `sha256=<hex>` of `HMAC-SHA256(secret, body)` — what the consumer recomputes
/// over the raw bytes it received and compares (in constant time) to the header.
fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts a key of any length");
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256={hex}")
}

#[async_trait]
impl DigestSender for WebhookDigestSender {
    async fn send(&self, webhook_url: &str, digest: &Digest) -> Result<(), PortError> {
        // SSRF guard (ADR-055): the URL is user-supplied, so refuse a host that
        // resolves to an internal address before making any request.
        ensure_public_host(webhook_url, self.allow_private).await?;
        // Serialize once so the signature covers exactly the bytes we send.
        let body = serde_json::to_vec(digest)
            .map_err(|e| PortError(format!("digest serialization failed: {e}")))?;
        let mut request = self
            .client
            .post(webhook_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone());
        if let Some(secret) = &self.secret {
            request = request.header(SIGNATURE_HEADER, sign(secret, &body));
        }
        let response = request
            .send()
            .await
            .map_err(|e| PortError(format!("webhook unreachable: {e}")))?;
        if !response.status().is_success() {
            return Err(PortError(format!("webhook returned {}", response.status())));
        }
        Ok(())
    }
}

/// No-op sender for tests and for wiring points that do not deliver digests.
#[derive(Default)]
pub struct NoopDigestSender;

#[async_trait]
impl DigestSender for NoopDigestSender {
    async fn send(&self, _webhook_url: &str, _digest: &Digest) -> Result<(), PortError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::routing::post;
    use axum::Router;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    /// Captures each request's signature header (if any) and its raw body.
    type Received = Arc<Mutex<Vec<(Option<String>, String)>>>;

    async fn spawn_stub(status: u16) -> (String, Received) {
        let received: Received = Arc::default();
        let app = Router::new()
            .route(
                "/hook",
                post(
                    move |State(received): State<Received>,
                          headers: axum::http::HeaderMap,
                          body: String| async move {
                        let sig = headers
                            .get(SIGNATURE_HEADER)
                            .and_then(|v| v.to_str().ok())
                            .map(String::from);
                        received.lock().unwrap().push((sig, body));
                        axum::http::StatusCode::from_u16(status).unwrap()
                    },
                ),
            )
            .with_state(received.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/hook"), received)
    }

    fn a_digest() -> Digest {
        Digest {
            recurring_search_id: Uuid::new_v4(),
            job_id: Uuid::new_v4(),
            keyword: "rust releases".into(),
            new_count: 1,
            new_results: vec![crate::domain::ports::DigestEntry {
                title: "Rust 1.99".into(),
                url: "https://ex.com/rust".into(),
                published_at: None,
            }],
        }
    }

    #[tokio::test]
    async fn posts_the_digest_json_unsigned_by_default() {
        let (url, received) = spawn_stub(204).await;

        WebhookDigestSender::new(None, true)
            .send(&url, &a_digest())
            .await
            .unwrap();

        let entries = received.lock().unwrap();
        let (sig, raw) = &entries[0];
        assert!(sig.is_none(), "no signature without a secret");
        let body: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(body["keyword"], "rust releases");
        assert_eq!(body["new_results"][0]["title"], "Rust 1.99");
    }

    #[tokio::test]
    async fn signs_the_body_with_hmac_sha256_when_a_secret_is_set() {
        let (url, received) = spawn_stub(204).await;

        WebhookDigestSender::new(Some("s3cret".into()), true)
            .send(&url, &a_digest())
            .await
            .unwrap();

        let entries = received.lock().unwrap();
        let (sig, raw) = &entries[0];
        let sig = sig.as_deref().expect("signature header present");
        // Exactly the consumer's check: recompute over the received bytes.
        assert_eq!(sig, sign("s3cret", raw.as_bytes()));
        assert!(sig.starts_with("sha256="));
    }

    #[test]
    fn sign_matches_a_known_hmac_sha256_vector() {
        // RFC-style vector — guards the algorithm itself, not just self-consistency.
        assert_eq!(
            sign("key", b"The quick brown fox jumps over the lazy dog"),
            "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8",
        );
    }

    #[tokio::test]
    async fn non_success_statuses_surface_as_errors() {
        let (url, _) = spawn_stub(500).await;
        let err = WebhookDigestSender::new(None, true)
            .send(&url, &a_digest())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("500"));
    }

    // ---------------------------------------------------------------- SSRF guard (ADR-055)

    #[test]
    fn blocks_non_public_ips_and_allows_public_ones() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254", // cloud metadata
            "100.64.0.1",      // CGNAT
            "0.0.0.0",
            "::1",
            "fe80::1", // link-local
            "fc00::1", // unique local
        ] {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip} must be blocked");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!is_blocked_ip(ip.parse().unwrap()), "{ip} must be allowed");
        }
    }

    #[tokio::test]
    async fn refuses_a_webhook_resolving_to_an_internal_host() {
        // Literal internal addresses and `localhost` (via /etc/hosts) — offline.
        for url in [
            "http://127.0.0.1:6379/",
            "http://10.0.0.5/",
            "http://localhost/hook",
        ] {
            assert!(
                ensure_public_host(url, false).await.is_err(),
                "{url} must be refused"
            );
        }
        // A public literal is allowed (no DNS needed).
        assert!(ensure_public_host("http://8.8.8.8/hook", false)
            .await
            .is_ok());
        // The escape hatch (tests) skips the check.
        assert!(ensure_public_host("http://127.0.0.1/", true).await.is_ok());
    }

    #[tokio::test]
    async fn send_refuses_an_internal_webhook_before_any_request() {
        let err = WebhookDigestSender::new(None, false)
            .send("http://169.254.169.254/latest/meta-data/", &a_digest())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("non-public"));
    }

    // ---------------------------------------------------------------- DNS-rebinding backstop (ADR-056)

    #[tokio::test]
    async fn resolver_blocks_hosts_resolving_to_a_non_public_address() {
        use std::str::FromStr;
        // `localhost` resolves to a loopback address (offline, via /etc/hosts):
        // the connect-time resolver refuses it even if a pre-flight was fooled.
        // (`Addrs` is not Debug, so assert on the Result rather than unwrap_err.)
        let refused = PublicOnlyResolver
            .resolve(Name::from_str("localhost").unwrap())
            .await;
        let err = refused.err().expect("localhost must be refused");
        assert!(err.to_string().contains("non-public"));

        // A public literal passes (no DNS lookup needed).
        let mut addrs = PublicOnlyResolver
            .resolve(Name::from_str("8.8.8.8").unwrap())
            .await
            .expect("a public literal resolves");
        assert!(addrs.all(|a| !is_blocked_ip(a.ip())));
    }
}
