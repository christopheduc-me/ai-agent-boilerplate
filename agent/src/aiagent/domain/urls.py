"""URL canonicalization (ADR-034), stdlib only — pure domain.

Search providers return the same article under cosmetically different URLs
(tracking parameters, fragments, host casing…), which double-counts results
and breaks the recurring-search memory (ADR-033). `normalize_url` produces the
comparison key; the **displayed** URL always stays the original.

Choices (deliberately conservative):
- scheme and host lowercased; default ports (:80 http, :443 https) dropped;
- fragment dropped (never sent to the server);
- known tracking parameters removed (utm_*, click ids…), the rest kept and
  sorted so parameter order stops mattering;
- trailing slash trimmed (except the root path);
- anything unparseable is returned unchanged — a weird URL must never crash
  a job, it just deduplicates less well.
"""

from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

#: Query parameters that identify campaigns/clicks, not content.
_TRACKING_EXACT = frozenset(
    {
        "gclid",
        "fbclid",
        "igshid",
        "msclkid",
        "twclid",
        "yclid",
        "mc_cid",
        "mc_eid",
        "ref",
        "ref_src",
        "spm",
        "s_kwcid",
    }
)
_TRACKING_PREFIXES = ("utm_",)


def _is_tracking(name: str) -> bool:
    lowered = name.lower()
    return lowered in _TRACKING_EXACT or lowered.startswith(_TRACKING_PREFIXES)


def normalize_url(url: str) -> str:
    """The canonical comparison key for a result URL."""
    try:
        parts = urlsplit(url.strip())
        host = parts.hostname
    except ValueError:
        return url
    if not parts.scheme or host is None:
        return url

    scheme = parts.scheme.lower()
    netloc = host.lower()
    default_port = {"http": 80, "https": 443}.get(scheme)
    if parts.port is not None and parts.port != default_port:
        netloc = f"{netloc}:{parts.port}"

    path = parts.path or "/"
    if len(path) > 1 and path.endswith("/"):
        path = path.rstrip("/")

    kept = [
        (k, v) for k, v in parse_qsl(parts.query, keep_blank_values=True) if not _is_tracking(k)
    ]
    query = urlencode(sorted(kept))

    return urlunsplit((scheme, netloc, path, query, ""))
