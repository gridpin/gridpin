"""The loadable SQLite extension (needs ./build.sh and code/data/mc.bin)."""
import json
import os
import sqlite3
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parents[1]
CODE = HERE.parents[1]
EXT = HERE / ("gridpin_sqlite.dylib" if sys.platform == "darwin" else "gridpin_sqlite.so")
SHEET = Path(os.environ.get("GRIDPIN_TEST_SHEET", CODE / "data" / "mc.bin"))
pytestmark = pytest.mark.skipif(not EXT.exists() or not SHEET.exists(), reason="run ./build.sh and make mc")


@pytest.fixture
def db():
    c = sqlite3.connect(":memory:")
    c.enable_load_extension(True)
    c.load_extension(str(EXT.with_suffix("")))
    c.execute("SELECT gridpin_open(?)", (str(SHEET),))
    return c


def test_geocode_returns_the_answer_as_json_with_its_level(db):
    row = json.loads(db.execute("SELECT gridpin_geocode('boulevard des moulins 27, monaco')").fetchone()[0])
    assert row["housenumber"] == "27" and row["precision"] == "house" and row["match_level"] == "PointAddress"


def test_lat_and_lon_are_numbers_and_nothing_found_is_null(db):
    lat, lon = db.execute("SELECT gridpin_lat('boulevard des moulins 27, monaco'), gridpin_lon('boulevard des moulins 27, monaco')").fetchone()
    assert 43.7 < lat < 43.8 and 7.4 < lon < 7.5
    assert db.execute("SELECT gridpin_lat(NULL)").fetchone()[0] is None


def test_a_table_of_addresses(db):
    db.execute("CREATE TABLE t(a TEXT)")
    db.executemany("INSERT INTO t VALUES (?)", [("boulevard des moulins 27, monaco",), ("avenue de la costa 1 monaco",)])
    levels = [r[0] for r in db.execute("SELECT json_extract(gridpin_geocode(a), '$.match_level') FROM t")]
    assert all(levels)


@pytest.fixture
def bare():
    """A connection with the extension loaded and no sheet opened yet."""
    c = sqlite3.connect(":memory:")
    c.enable_load_extension(True)
    c.load_extension(str(EXT.with_suffix("")))
    return c


@pytest.mark.parametrize("fn", ["gridpin_geocode", "gridpin_lat", "gridpin_lon"])
def test_a_call_before_gridpin_open_says_what_to_do(bare, fn):
    # sqlite3_result_null after sqlite3_result_error used to wipe the message: the user got an empty error
    with pytest.raises(sqlite3.OperationalError) as e:
        bare.execute(f"SELECT {fn}('boulevard des moulins 27, monaco')").fetchone()
    assert "gridpin_open(" in str(e.value) and "gridpin_load" not in str(e.value)


@pytest.mark.parametrize("fn", ["gridpin_geocode", "gridpin_lat", "gridpin_lon"])
def test_invalid_utf8_is_an_error_with_words(db, fn):
    with pytest.raises(sqlite3.OperationalError) as e:
        db.execute(f"SELECT {fn}(CAST(x'c328' AS TEXT))").fetchone()
    assert str(e.value).startswith("gridpin: ") and len(str(e.value)) > len("gridpin: ")


def test_switching_sheets_names_functions_that_exist_and_reset_allows_it(db):
    other = os.path.join(str(SHEET.parent), ".", SHEET.name)   # the same sheet under another path
    with pytest.raises(sqlite3.OperationalError) as e:
        db.execute("SELECT gridpin_open(?)", (other,)).fetchone()
    msg = str(e.value)
    assert "gridpin_reset()" in msg and "gridpin_open(" in msg and "gridpin_load" not in msg
    assert db.execute("SELECT gridpin_reset()").fetchone()[0] == "index unloaded"
    db.execute("SELECT gridpin_open(?)", (other,)).fetchone()
    assert db.execute("SELECT gridpin_lat('boulevard des moulins 27, monaco')").fetchone()[0] > 43.7
