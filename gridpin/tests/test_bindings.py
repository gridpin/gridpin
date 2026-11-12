"""End-to-end tests for the Python bindings (pyo3 wheel).

Run after `maturin develop`:  pytest gridpin/tests/
Uses the small Monaco smoke sheet the CI pipeline builds; set GRIDPIN_TEST_SHEET
to point at another .bin (e.g. a country sheet) to run against real data too.
"""
import os
import pathlib

import pytest

gridpin = pytest.importorskip("gridpin")
# Run from the repository root, the gridpin/ crate folder is an importable namespace package named
# "gridpin": importorskip passes without the wheel. The wheel is what defines Geocoder.
if not hasattr(gridpin, "Geocoder"):
    pytest.skip("gridpin wheel not installed (run `maturin develop`)", allow_module_level=True)

CODE = pathlib.Path(__file__).resolve().parents[2]
SHEET = pathlib.Path(os.environ.get("GRIDPIN_TEST_SHEET", CODE / "data" / "mc.bin"))


@pytest.fixture(scope="module")
def geo():
    if not SHEET.exists():
        pytest.skip(f"no test sheet at {SHEET} (build the Monaco smoke sheet: make mc)")
    return gridpin.Geocoder(str(SHEET))


def test_geocode_returns_list_of_dicts(geo):
    hits = geo.geocode("boulevard des moulins 27 monaco", 3)
    assert isinstance(hits, list)
    assert len(hits) >= 1
    top = hits[0]
    # documented response contract, identical across CLI / DuckDB / Python
    for key in ("lat", "lon", "precision", "score", "confidence", "street", "commune"):
        assert key in top
    assert isinstance(top["lat"], float) and isinstance(top["lon"], float)


def test_every_answer_carries_the_weak_verdict(geo):
    # the engine's own "check before use": a city-level answer is weak, an exact house is not
    assert geo.geocode("monaco", 1)[0]["weak"] is True
    assert geo.geocode("boulevard des moulins 27 monaco", 1)[0]["weak"] is False


def test_parse_reports_the_reading_and_the_answer(geo):
    r = geo.parse("boulevard des moulins 27 monaco")
    assert (r["house_number"], r["words"]) == (27, "boulevard des moulins monaco")
    assert r["answer"]["housenumber"] == "27"
    assert geo.parse("xyzzy plugh nonexistent street 999")["answer"] is None


def test_same_address_levels(geo):
    assert geo.same_address("boulevard des moulins 27 monaco", "Bd des Moulins 27, Monaco")["level"] == "house"
    assert geo.same_address("boulevard des moulins 27 monaco", "boulevard des moulins 29 monaco")["level"] == "street"
    unknown = geo.same_address("boulevard des moulins 27 monaco", "xyzzy plugh 999")
    assert (unknown["level"], unknown["text"]) == ("unknown", "different_text")


def test_every_answer_carries_the_match_level(geo):
    city = geo.geocode("monaco", 1)[0]
    assert (city["match_level"], city["match_status"]) == ("Locality", "T")
    house = geo.geocode("boulevard des moulins 27 monaco", 1)[0]
    assert (house["match_level"], house["match_status"]) == ("PointAddress", "M")


def test_fields_answer_like_their_line(geo):
    [[by_fields], [other], nothing] = geo.geocode_fields_many(
        [("boulevard des moulins", "27", "monaco", None, None),
         ("27 boulevard des moulins monaco", None, "", None, None),
         ("xyzzy plugh", "999", "nowhere", None, None)], 1)
    top = geo.geocode("boulevard des moulins 27 monaco", 1)[0]
    assert (by_fields["street"], by_fields["housenumber"]) == (top["street"], top["housenumber"])
    assert (other["lat"], other["lon"]) == (geo.geocode("27 boulevard des moulins monaco", 1)[0]["lat"],
                                            geo.geocode("27 boulevard des moulins monaco", 1)[0]["lon"])
    assert nothing == []


def test_houses_are_a_table_of_the_sheet(geo):
    rows = geo.houses(street="boulevard des moulins", city="monaco")
    assert len(rows) > 5
    assert {r["street"] for r in rows} == {rows[0]["street"]}
    assert set(rows[0]) >= {"lat", "lon", "street", "housenumber", "postcode", "commune"}
    assert len(geo.houses(city="monaco", limit=5)) == 5
    top = geo.geocode("boulevard des moulins 27 monaco", 1)[0]
    near = geo.houses(near=(top["lat"], top["lon"], 5.0))
    assert any(r["housenumber"] == top["housenumber"] and r["street"] == top["street"] for r in near)
    box = geo.houses(bbox=(top["lon"] - 0.0005, top["lat"] - 0.0005, top["lon"] + 0.0005, top["lat"] + 0.0005))
    assert len(box) >= len(near) > 0
    assert len(geo.houses(limit=3)) == 3, "no option: the whole sheet, up to the limit"
    with pytest.raises(ValueError):
        geo.houses(street="boulevard des moulins")  # a street name alone is many streets
    with pytest.raises(ValueError):
        geo.houses(bbox=(10.0, 0.0, 1.0, 0.0))


def test_a_sample_is_repeatable_and_of_the_area(geo):
    a, b = geo.sample(5, seed=7), geo.sample(5, seed=7)
    assert a == b and len(a) == 5
    assert a != geo.sample(5, seed=8)
    in_city = geo.sample(1000, seed=1, street="boulevard des moulins", city="monaco")
    assert {r["street"] for r in in_city} == {in_city[0]["street"]}


def test_geocode_empty_on_garbage(geo):
    # a made-up street must return an empty list, never a fabricated hit
    assert geo.geocode("xyzzy plugh nonexistent street 999", 1) == []


def test_geocode_many_is_list_of_lists(geo):
    # honours k: one list of up to k hits per query (regression for the "ignored 2..k" bug)
    out = geo.geocode_many(["boulevard des moulins 27 monaco", "totally not an address"], 2)
    assert isinstance(out, list) and len(out) == 2
    assert isinstance(out[0], list) and len(out[0]) >= 1
    assert out[1] == []  # no match -> empty list, not None


def test_geocode_many_threads_zero_does_not_crash(geo, monkeypatch):
    # GRIDPIN_THREADS=0 must clamp to >=1, not build a 0-thread pool and panic
    monkeypatch.setenv("GRIDPIN_THREADS", "0")
    out = gridpin.Geocoder(str(SHEET)).geocode_many(["boulevard des moulins 27 monaco"], 1)
    assert len(out) == 1


def test_reverse_returns_distance(geo):
    hits = geo.reverse(43.7391, 7.4266, 1)
    assert isinstance(hits, list)
    if hits:  # Monaco is tiny; a nearby point should resolve
        assert "distance_m" in hits[0]


def test_reverse_contract_street_and_housenumber_split(geo):
    # at the Python boundary: reverse gives a clean street name plus a separate
    # housenumber field — the number must NOT be glued into `street`.
    hits = geo.reverse(43.7391, 7.4266, 1)
    if hits:
        h = hits[0]
        assert "housenumber" in h, "reverse must expose a housenumber field"
        assert h["street"], "street must be present"
        # the number is not part of the street name
        assert not h["street"].rstrip().endswith(str(h["housenumber"]))


def test_reverse_rejects_non_finite_and_out_of_range(geo):
    # at the Python boundary: NaN/Inf/out-of-range coordinates are a CLEAR error, never a
    # panic and never a silent empty "success". try_reverse is the single strict entry point;
    # the wheel must surface it as a Python exception, identically to the CLI and DuckDB.
    import math
    for lat, lon in [(math.nan, 7.42), (43.7, math.inf), (math.nan, math.nan),
                     (91.0, 7.42), (43.7, 200.0), (-100.0, -300.0)]:
        with pytest.raises(ValueError) as e:
            geo.reverse(lat, lon, 1)
        msg = str(e.value).lower()
        assert "reverse" in msg and ("finite" in msg or "range" in msg), msg
    # a valid in-range point still works after the rejections (state not corrupted)
    assert isinstance(geo.reverse(43.7391, 7.4266, 1), list)


def test_forward_forwards_the_matched_housenumber(geo):
    # Build a forward query from an actual binding-level reverse result.  This proves the
    # PyO3 dict keeps the public field and avoids assuming which Monaco source address is
    # present in a particular smoke-sheet revision.
    reverse = geo.reverse(43.7391, 7.4266, 1)
    if not reverse:
        pytest.skip("the smoke sheet has no reverse result at the fixture coordinate")
    address = reverse[0]
    hits = geo.geocode(
        f"{address['housenumber']} {address['street']} {address['commune']}", 1
    )
    assert hits, "forward lookup of a reverse address must resolve"
    assert hits[0].get("housenumber") == address["housenumber"]


def test_corrupt_sheet_raises_not_panics(tmp_path):
    # a truncated/garbage file must raise a clean exception, never abort the process
    bad = tmp_path / "bad.bin"
    bad.write_bytes(b"not a gpc0 file at all")
    with pytest.raises(Exception):
        gridpin.Geocoder(str(bad))
    empty = tmp_path / "empty.bin"
    empty.write_bytes(b"")
    with pytest.raises(Exception):
        gridpin.Geocoder(str(empty))


@pytest.mark.skipif(not hasattr(os, "fork"), reason="fork() is POSIX-only")
def test_geocode_many_survives_fork(geo):
    # regression: the rayon pool is keyed by PID so a forked multiprocessing worker
    # rebuilds it instead of hanging forever on threads that did not survive the fork.
    # Warm the pool in the parent first (this is what made the old bug deterministic).
    geo.geocode_many(["boulevard des moulins 27 monaco"] * 4, 1)
    r, w = os.pipe()
    pid = os.fork()
    if pid == 0:  # child
        os.close(r)
        try:
            n = len(geo.geocode_many(["boulevard des moulins 27 monaco"] * 4, 1))
            os.write(w, b"1" if n == 4 else b"0")
        except BaseException:
            os.write(w, b"x")
        finally:
            os._exit(0)
    # parent: the child must answer quickly, not hang on a dead pool
    os.close(w)
    import select
    ready, _, _ = select.select([r], [], [], 20)
    assert ready, "forked child hung in geocode_many (pool not rebuilt after fork)"
    result = os.read(r, 1)
    os.close(r)
    os.waitpid(pid, 0)
    assert result == b"1", f"forked child did not return the expected result: {result!r}"


def test_a_sheet_of_own_places_is_asked_before_the_country(tmp_path):
    if not SHEET.exists():
        pytest.skip(f"no test sheet at {SHEET} (build the Monaco smoke sheet: make mc)")
    if not hasattr(gridpin, "build_places"):
        pytest.skip("engine before 0.9: no own sheets")
    places = tmp_path / "site.csv"
    places.write_text("name,lat,lon,group,aliases\nNorth Depot,43.7400,7.4200,Site,Depot A\n")
    own = tmp_path / "site.bin"
    assert gridpin.build_places(str(places), str(own)) == 1
    g = gridpin.Geocoder(str(SHEET), also=str(own))
    top = g.geocode("North Depto", 1)[0]
    assert "own_sheet" in top["flags"] and top["street"] == "North Depot"
    assert (top["lat"], top["lon"]) == (43.74, 7.42)
    assert g.geocode("Depot A, Monaco", 1)[0]["street"] == "North Depot", "an alias, the town dropped"
    plain = gridpin.Geocoder(str(SHEET))
    address = "boulevard des moulins 27 monaco"
    assert g.geocode(address, 1) == plain.geocode(address, 1), "an address goes the old way"
    with pytest.raises(ValueError, match="line 3"):
        places.write_text("name,lat,lon\nDepot,1,2\ndepot,3,4\n")
        gridpin.build_places(str(places), str(tmp_path / "bad.bin"))


def test_extract_finds_the_address_in_a_text(geo):
    found = geo.extract("Please send it to boulevard des moulins 27, Monaco before Friday.")
    assert [f["level"] for f in found] == ["house"]
    assert found[0]["hit"]["housenumber"] == "27"
    text = "Please send it to boulevard des moulins 27, Monaco before Friday."
    assert text.encode()[found[0]["start"]:found[0]["end"]].decode() == found[0]["text"]
    assert geo.extract("no address here, only words") == []
