"""Agent-facing tools over the real engine and the Monaco smoke sheet (make mc)."""
import os
from pathlib import Path

import pytest

# From the repository root the gridpin/ crate folder is a namespace package named "gridpin";
# only the engine wheel defines Geocoder.
if not hasattr(pytest.importorskip("gridpin"), "Geocoder"):
    pytest.skip("gridpin engine wheel not installed", allow_module_level=True)
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
    assert {"precision", "confidence", "street", "commune", "weak"} <= set(top)
    assert isinstance(top["weak"], bool)
    assert isinstance(top["match_level"], str) and top["match_status"] in ("M", "T")
    assert len(out["results"]) <= 3


def test_extras_are_passed_to_the_engine_only_when_asked(session):
    plain = session.geocode(ADDRESS, "fr")["results"][0]
    assert "extras" not in plain
    top = session.geocode(ADDRESS, "fr", extras="h3,codes")["results"][0]
    assert set(top["extras"]) == {"h3", "plus_code", "mgrs", "dms"}
    back = session.reverse(top["lat"], top["lon"], "fr", extras="geohash:6")["results"][0]
    assert len(back["extras"]["geohash"]) == 6
    with pytest.raises(tools.ToolError, match="unknown field"):
        session.geocode(ADDRESS, "fr", extras="zip")


def test_geocode_says_what_kind_of_answer_it_is(session):
    if not hasattr(session.geocoder("fr")[1], "geocode_or_ask"):
        pytest.skip("engine before 0.9: no outcome")
    assert session.geocode(ADDRESS, "fr")["kind"] == "answer"
    silent = session.geocode("xyzzy plugh nonexistent street 999", "fr")
    assert silent["results"] == [] and silent["kind"] == "none" and silent["reason"] in (
        "no_street", "no_house", "city_unknown", "outside_sheet", "unparsed")


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


def test_the_own_sheet_setting_puts_ones_places_before_the_country(session, tmp_path, monkeypatch):
    engine = pytest.importorskip("gridpin")
    if not hasattr(engine, "build_places"):
        pytest.skip("engine before 0.9: no own sheets")
    places = tmp_path / "site.csv"
    places.write_text("name,lat,lon\nNorth Depot,43.7400,7.4200\n")
    engine.build_places(str(places), str(tmp_path / "site.bin"))
    monkeypatch.setenv(tools.OWN_SHEET, str(tmp_path / "site.bin"))
    top = session.geocode("North Depot", "fr")["results"][0]
    assert "own_sheet" in top["flags"] and top["street"] == "North Depot"
    monkeypatch.setenv(tools.OWN_SHEET, str(tmp_path / "missing.bin"))
    with pytest.raises(tools.ToolError, match="GRIDPIN_OWN_SHEET"):
        tools.Session(open_sheet=lambda code: SHEET).geocode("North Depot", "fr")


def test_extract_finds_the_address_or_says_the_engine_is_too_old(session):
    text = f"Please deliver to {ADDRESS} tomorrow."
    _, geo = session.geocoder("fr")
    if not hasattr(geo, "extract"):
        with pytest.raises(tools.ToolError, match="upgrade"):
            session.extract(text, "fr")
        return
    out = session.extract(text, "fr")
    assert [r["hit"]["housenumber"] for r in out["results"]] == ["27"]


def test_extract_refuses_an_empty_text(session):
    with pytest.raises(tools.ToolError, match="non-empty"):
        session.extract("  ", "fr")
