"""URL canonicalization (ADR-034)."""

from aiagent.domain.urls import normalize_url


def test_tracking_parameters_are_stripped_and_the_rest_sorted() -> None:
    url = "https://ex.com/article?utm_source=x&b=2&a=1&fbclid=abc&UTM_campaign=y"
    assert normalize_url(url) == "https://ex.com/article?a=1&b=2"


def test_host_case_ports_fragment_and_trailing_slash_are_canonical() -> None:
    assert (
        normalize_url("HTTPS://Ex.COM:443/Article/#section")
        == "https://ex.com/Article"  # path case is meaningful, host case is not
    )
    assert normalize_url("http://ex.com:8080/") == "http://ex.com:8080/"
    assert normalize_url("https://ex.com") == "https://ex.com/"


def test_equivalent_urls_share_one_key() -> None:
    variants = [
        "https://ex.com/post?id=7&utm_medium=rss",
        "https://EX.com:443/post/?id=7",
        "https://ex.com/post?id=7#comments",
    ]
    keys = {normalize_url(v) for v in variants}
    assert keys == {"https://ex.com/post?id=7"}


def test_meaningful_parts_are_preserved() -> None:
    # A non-tracking query parameter distinguishes two different pages.
    assert normalize_url("https://ex.com/p?page=1") != normalize_url("https://ex.com/p?page=2")


def test_garbage_is_returned_unchanged() -> None:
    for weird in ("not a url", "mailto:x@y.z", "//no-scheme.com/x", "http://[broken"):
        assert normalize_url(weird) == weird
