"""End-to-end tests for the Python bindings (pyo3 wheel).

Run after `maturin develop`:  pytest gridpin/tests/
Uses the small Monaco smoke sheet the CI pipeline builds; set GRIDPIN_TEST_SHEET
to point at another .bin (e.g. a country sheet) to run against real data too.
"""
import os
import pathlib

import pytest

gridpin = pytest.importorskip("gridpin")

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
