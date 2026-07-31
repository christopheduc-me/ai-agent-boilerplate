"""Page-declared publication dates (ADR-035): extraction + HTTP adapter."""

from datetime import UTC, datetime

import httpx
import respx

from aiagent.adapters.page import HttpPageDateFetcher, extract_published_date


def test_jsonld_date_published_is_extracted() -> None:
    html = """<html><head>
      <script type="application/ld+json">
        {"@type": "NewsArticle", "datePublished": "2026-03-05T08:30:00Z"}
      </script></head><body>hi</body></html>"""
    assert extract_published_date(html) == datetime(2026, 3, 5, 8, 30, tzinfo=UTC)


def test_jsonld_graph_and_list_shapes_are_walked() -> None:
    html = """<script type="application/ld+json">
      {"@graph": [{"@type": "WebSite"}, {"@type": "Article", "datePublished": "2025-11-02"}]}
    </script>"""
    assert extract_published_date(html) == datetime(2025, 11, 2, tzinfo=UTC)
    listed = """<script type="application/ld+json">
      [{"@type": "Article", "datePublished": "2025-11-03"}]
    </script>"""
    assert extract_published_date(listed) == datetime(2025, 11, 3, tzinfo=UTC)


def test_opengraph_is_the_fallback_and_jsonld_wins() -> None:
    og_only = '<meta property="article:published_time" content="2024-08-01T10:00:00+02:00">'
    assert extract_published_date(og_only) == datetime(2024, 8, 1, 8, 0, tzinfo=UTC)

    both = """<script type="application/ld+json">{"datePublished": "2024-01-01"}</script>
      <meta property="article:published_time" content="2020-01-01">"""
    assert extract_published_date(both) == datetime(2024, 1, 1, tzinfo=UTC)


def test_garbage_metadata_means_no_date() -> None:
    for html in (
        "<p>no metadata at all</p>",
        '<script type="application/ld+json">not json</script>',
        '<script type="application/ld+json">{"datePublished": "someday"}</script>',
        '<meta property="article:published_time" content="">',
        "<html><head><script type=",  # truncated / malformed
    ):
        assert extract_published_date(html) is None


@respx.mock
def test_fetcher_reads_the_page_and_degrades_silently() -> None:
    respx.get("https://ex.com/dated").mock(
        return_value=httpx.Response(
            200,
            text='<script type="application/ld+json">{"datePublished": "2026-02-02"}</script>',
        )
    )
    respx.get("https://ex.com/missing").mock(return_value=httpx.Response(404))
    respx.get("https://ex.com/down").mock(side_effect=httpx.ConnectError("refused"))

    # Bypass the SSRF host check (ADR-055) so the test needs no real DNS.
    fetcher = HttpPageDateFetcher(client=httpx.Client(), is_public=lambda _host: True)

    assert fetcher.fetch_published_date("https://ex.com/dated") == datetime(2026, 2, 2, tzinfo=UTC)
    assert fetcher.fetch_published_date("https://ex.com/missing") is None
    assert fetcher.fetch_published_date("https://ex.com/down") is None


def test_is_public_host_blocks_internal_and_allows_public() -> None:
    from aiagent.adapters.page import is_public_host

    for host in ("127.0.0.1", "10.0.0.1", "192.168.1.1", "169.254.169.254", "localhost", "::1"):
        assert not is_public_host(host), f"{host} must be blocked"
    for host in ("8.8.8.8", "1.1.1.1"):
        assert is_public_host(host), f"{host} must be allowed"


def test_fetcher_refuses_an_internal_url() -> None:
    # Default guard (real resolution): loopback literals/localhost are refused
    # offline, and the fetch degrades to None before any request is made.
    fetcher = HttpPageDateFetcher(client=httpx.Client())
    assert fetcher.fetch_published_date("http://127.0.0.1:6379/") is None
    assert fetcher.fetch_published_date("http://localhost/admin") is None
