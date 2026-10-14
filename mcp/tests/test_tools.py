"""Agent-facing tools over the real engine and the Monaco smoke sheet (make mc)."""
import os
from pathlib import Path

import pytest

pytest.importorskip("gridpin")
from gridpin_mcp import sheets, tools  # noqa: E402

CODE = Path(__file__).resolve().parents[2]
SHEET = Path(os.environ.get("GRIDPIN_TEST_SHEET", CODE / "data" / "mc.bin"))
ADDRESS = "boulevard des moulins 27 monaco"


@pytest.fixture
def session():
    if not SHEET.exists():
        pytest.skip(f"no test sheet at {SHEET} (build the Monaco smoke sheet: make mc)")
    opened = []

    def open_sheet(code):
        opened.append(code)
        return SHEET

    s = tools.Session(open_sheet=open_sheet)
    s.opened = opened
    return s


def test_geocode_returns_engine_results_with_coordinates(session):
    out = session.geocode(ADDRESS, "France", k=3)
    assert out["country"] == "fr" and out["query"] == ADDRESS
    top = out["results"][0]
    assert isinstance(top["lat"], float) and isinstance(top["lon"], float)
    assert {"precision", "confidence", "street", "commune"} <= set(top)
    assert len(out["results"]) <= 3


def test_sheet_is_opened_once_per_country(session):
    session.geocode(ADDRESS, "fr")
    session.geocode(ADDRESS, "FR")
    session.reverse(43.74, 7.43, "France")
    assert session.opened == ["fr"]


def test_reverse_carries_distance(session):
    out = session.reverse(43.7397, 7.4265, "fr", k=2)
    assert out["results"] and all("distance_m" in r for r in out["results"])


def test_reverse_rejects_impossible_coordinates_with_a_readable_error(session):
    with pytest.raises(tools.ToolError):
        session.reverse(123.0, 7.0, "fr")


def test_batch_keeps_input_order_and_pairs_queries(session):
    # Asymmetric on purpose: a reversed or shifted pairing must change the answer.
    queries = [ADDRESS, "xyzzy plugh nonexistent street 999", "xyzzy plugh nonexistent street 998"]
    out = session.batch(queries, "fr")
    assert [row["query"] for row in out["results"]] == queries
    assert out["results"][0]["results"][0]["housenumber"] == "27"
    assert out["results"][1]["results"] == [] and out["results"][2]["results"] == []


@pytest.mark.parametrize("call, message", [
    (lambda s: s.geocode("   ", "fr"), "non-empty"),
    (lambda s: s.geocode(ADDRESS, "fr", k=0), "k must be"),
    (lambda s: s.geocode(ADDRESS, "fr", k=11), "k must be"),
    (lambda s: s.batch([], "fr"), "non-empty list"),
    (lambda s: s.batch([ADDRESS, 5], "fr"), "must be a string"),
    (lambda s: s.batch([ADDRESS] * (tools.MAX_BATCH + 1), "fr"), "at most 1000"),
    (lambda s: s.geocode(ADDRESS, "Poland"), "unknown country"),
])
def test_bad_input_is_a_tool_error_that_says_what_to_do(session, call, message):
    with pytest.raises(tools.ToolError) as err:
        call(session)
    assert message in str(err.value)


def test_batch_accepts_exactly_the_limit(session):
    out = session.batch([ADDRESS] * tools.MAX_BATCH, "fr")
    assert len(out["results"]) == tools.MAX_BATCH


def test_missing_sheet_becomes_tool_error_naming_the_download(tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path))
    with pytest.raises(tools.ToolError) as err:
        tools.Session().geocode(ADDRESS, "it")
    assert "gridpin-fetch it" in str(err.value)


def test_countries_reports_download_state(tmp_path, monkeypatch):
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path))
    out = tools.countries()
    assert out["release"] == sheets.RELEASE
    assert {c["code"] for c in out["countries"]} == set(sheets.SHEETS)
    assert not any(c["downloaded"] for c in out["countries"])
