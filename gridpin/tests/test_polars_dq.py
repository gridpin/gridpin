"""gridpin.polars and the pandera check of gridpin.dq against the Monaco smoke sheet.

CI installs polars and pandera in the `interfaces` job (ANSWERS §304 p. 3); without them these tests skip, and the
job that runs them is named in the skip reason. Reverse mutations that turn each test red: the namespace not
registered; one answer column lost or renamed; the row order of the batch not kept; a row without an answer not
marked "U"; the pandera check always passing (`>= 0.0`) or comparing the share the wrong way round.
"""
import os
import pathlib

import pytest

gridpin = pytest.importorskip("gridpin")
if not hasattr(gridpin, "Geocoder"):
    pytest.skip("gridpin wheel not installed (run `maturin develop`)", allow_module_level=True)
pl = pytest.importorskip("polars", reason="polars runs in the CI job `interfaces`")

CODE = pathlib.Path(__file__).resolve().parents[2]
SHEET = pathlib.Path(os.environ.get("GRIDPIN_TEST_SHEET", CODE / "data" / "mc.bin"))
if not SHEET.exists():
    pytest.skip(f"no test sheet at {SHEET} (build the Monaco smoke sheet: make mc)", allow_module_level=True)

import gridpin.dq  # noqa: E402
import gridpin.polars  # noqa: E402,F401  (registers df.gridpin)

HOUSE = "boulevard des moulins 27, monaco"
NOWHERE = "zzqx 999999"


def test_the_namespace_adds_every_answer_column_and_keeps_the_rows_in_order():
    df = pl.DataFrame({"id": [1, 2, 3], "address": [HOUSE, NOWHERE, "avenue de la costa 1 monaco"]})
    out = df.gridpin.geocode("address", str(SHEET))
    assert out.columns == ["id", "address", *gridpin.polars.GP_COLUMNS]
    assert out["id"].to_list() == [1, 2, 3]
    first = out.row(0, named=True)
    assert first["gp_housenumber"] == "27" and first["gp_match_level"] == "PointAddress"
    assert 43.7 < first["gp_lat"] < 43.8 and 7.4 < first["gp_lon"] < 7.5
    # the answer of the first row is the one the engine gives that row alone
    alone = gridpin.Geocoder(str(SHEET)).geocode(HOUSE, 1)[0]
    assert (first["gp_lat"], first["gp_lon"]) == (alone["lat"], alone["lon"])


def test_a_row_without_an_answer_is_unmatched_and_empty():
    out = pl.DataFrame({"address": [NOWHERE, None]}).gridpin.geocode("address", str(SHEET))
    for row in out.iter_rows(named=True):
        assert row["gp_match_status"] == "U"
        assert row["gp_lat"] is None and row["gp_match_level"] is None


def test_share_at_level_counts_the_rows_that_reach_the_level():
    assert gridpin.dq.share_at_level([HOUSE, NOWHERE], str(SHEET), "house") == 0.5
    assert gridpin.dq.share_at_level([], str(SHEET), "house") == 1.0
    with pytest.raises(ValueError):
        gridpin.dq.reaches(["PointAddress"], "planet")


@pytest.mark.parametrize("backend", ["polars", "pandas"])
def test_the_pandera_check_passes_and_fails_on_the_share(backend):
    """Under pandera.polars the check receives `PolarsData`, not a Series; `list(data)` of it read no addresses and the
    check failed at any share (found 11.11.2026 by this test). Mutation: `_column_values` = `list` — red for polars."""
    pa = pytest.importorskip(f"pandera.{backend}", reason="pandera runs in the CI job `interfaces`")
    frame = pl.DataFrame if backend == "polars" else pytest.importorskip("pandas").DataFrame
    check = gridpin.dq.pandera_check(str(SHEET), "house", at_least=0.5)
    strict = gridpin.dq.pandera_check(str(SHEET), "house", at_least=0.9)
    good = frame({"address": [HOUSE, NOWHERE]})
    pa.DataFrameSchema({"address": pa.Column(str, checks=check)}).validate(good)
    with pytest.raises(Exception, match="geocodes_to_house"):
        pa.DataFrameSchema({"address": pa.Column(str, checks=strict)}).validate(good)


WEAK_HOUSE = "27 boulevard"   # a PointAddress the engine marks weak on the Monaco sheet


def test_a_weak_answer_reaches_no_level():
    """Revision 0.9.0: the share counted weak answers (FR 0.90 with them, 0.73 without). Mutation: drop the `weak`
    condition in `_levels` — red here."""
    hit = gridpin.Geocoder(str(SHEET)).geocode(WEAK_HOUSE, 1)[0]
    assert hit["weak"] and hit["match_level"] == "PointAddress"   # the input still is what this test needs
    for level in gridpin.dq.LEVELS:
        assert gridpin.dq.share_at_level([WEAK_HOUSE], str(SHEET), level) == 0.0
    assert gridpin.dq.share_at_level([HOUSE, WEAK_HOUSE], str(SHEET), "house") == 0.5


def test_the_soda_sql_geocodes_once_and_leaves_weak_answers_out():
    sql = gridpin.dq.soda_check_sql("customers", "address", "street")
    assert sql.count("gridpin_geocode(") == 1 and "'$.weak'" in sql and "'StreetName'" in sql
    with pytest.raises(ValueError):
        gridpin.dq.soda_check_sql("customers", "address", "planet")


def test_no_promise_of_a_check_the_module_does_not_have():
    """docs.html and the docstring promised `gridpin.dq.ge_expectation`, which never existed (revision 0.9.0)."""
    assert "ge_expectation" not in (gridpin.dq.__doc__ or "")
    assert not hasattr(gridpin.dq, "ge_expectation")
