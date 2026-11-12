"""The Python layer of the wheel: gridpin.sheets, gridpin.pandas (geocode_df / reverse_df / df.gridpin) and
gridpin.geopy.GridPin, over the Monaco smoke sheet (make mc).

Run after the wheel is installed (`make bindings`): pytest gridpin/tests/
"""
import json
import os
import pathlib
import re
import subprocess
import sys
import tomllib

import pytest

gridpin = pytest.importorskip("gridpin")
# From the repository root the gridpin/ crate folder is a namespace package named "gridpin": the wheel
# is what defines Geocoder and the sheets module.
if not hasattr(gridpin, "Geocoder") or not hasattr(gridpin, "sheets"):
    pytest.skip("gridpin wheel with the Python layer not installed (make bindings)", allow_module_level=True)

CODE = pathlib.Path(__file__).resolve().parents[2]
CRATE = CODE / "gridpin"
SHEET = CODE / "data" / "mc.bin"
REGISTRY = json.loads((CRATE / "sheets.json").read_text(encoding="utf-8"))
ADDRESS = "boulevard des moulins 27 monaco"
NOWHERE = "xyzzy plugh nonexistent street 999"
SEVERAL = "avenue monaco"  # three candidates on the smoke sheet


@pytest.fixture(scope="module")
def geo():
    if not SHEET.exists():
        pytest.skip(f"no test sheet at {SHEET} (make mc)")
    return gridpin.Geocoder(str(SHEET))


@pytest.fixture
def home(tmp_path, monkeypatch):
    """An empty sheet cache: GRIDPIN_HOME is read on every call, like the CLI."""
    monkeypatch.setenv("GRIDPIN_HOME", str(tmp_path))
    monkeypatch.delenv("GRIDPIN_OFFLINE", raising=False)
    return tmp_path


def _python(code: str) -> subprocess.CompletedProcess:
    """Run code in a fresh interpreter that sees the same gridpin as this one."""
    env = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1",
           "PYTHONPATH": os.pathsep.join(p for p in (str(pathlib.Path(gridpin.__file__).parents[1]),
                                                      os.environ.get("PYTHONPATH")) if p)}
    return subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, env=env, timeout=120)


# ---- the wheel: mixed layout, no new dependencies --------------------------------------------------------------

def test_the_wheel_needs_nothing_and_offers_pandas_and_geopy_as_extras():
    project = tomllib.loads((CRATE / "pyproject.toml").read_text(encoding="utf-8"))
    assert "dependencies" not in project["project"], "the engine wheel installs with no dependencies"
    extras = project["project"]["optional-dependencies"]
    assert set(extras) == {"pandas", "geopy"}
    assert [re.split(r"[<>=]", d)[0] for d in extras["pandas"]] == ["pandas"]
    assert [re.split(r"[<>=]", d)[0] for d in extras["geopy"]] == ["geopy"]
    maturin = project["tool"]["maturin"]
    assert (maturin["python-source"], maturin["module-name"]) == ("python", "gridpin._gridpin")


def test_import_gridpin_loads_neither_pandas_nor_geopy():
    r = _python("import sys, gridpin; gridpin.Geocoder; gridpin.sheets.list();"
                "print(sorted(m for m in ('pandas', 'geopy') if m in sys.modules))")
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "[]"


@pytest.mark.parametrize("module, extra", [("gridpin.pandas", "pandas"), ("gridpin.geopy", "geopy")])
def test_a_helper_without_its_package_names_the_extra_to_install(module, extra):
    r = _python(f"import sys; sys.modules[{extra!r}] = None\n"
                f"try:\n    import {module}\nexcept ImportError as e:\n    print(e)")
    assert r.returncode == 0, r.stderr
    assert f'pip install "gridpin[{extra}]"' in r.stdout


def test_geocode_df_is_reachable_from_the_top_level_module():
    pytest.importorskip("pandas")
    import gridpin.pandas as gp_pandas

    assert gridpin.geocode_df is gp_pandas.geocode_df and gridpin.reverse_df is gp_pandas.reverse_df
    with pytest.raises(AttributeError):
        gridpin.no_such_helper


# ---- gridpin.sheets: the engine's registry, cache folder and check ---------------------------------------------

def test_the_sheet_list_is_the_registry_compiled_into_the_engine():
    assert gridpin.sheets.RELEASE == REGISTRY["release"]
    assert gridpin.sheets.BASE_URL == REGISTRY["base_url"]
    listed = [(s.code, s.name, s.file, s.size, s.sha256, s.url) for s in gridpin.sheets.list()]
    pinned = [(s["code"], s["name"], s["file"], s["size"], s["sha256"],
               f'{REGISTRY["base_url"]}/{REGISTRY["release"]}/{s["file"]}') for s in REGISTRY["sheets"]]
    assert listed == pinned


def test_countries_resolve_by_code_or_name_and_an_unknown_one_lists_the_known():
    first = REGISTRY["sheets"][0]
    assert gridpin.sheets.resolve(first["code"].upper()).code == first["code"]
    assert gridpin.sheets.resolve(f' {first["name"].lower()} ').code == first["code"]
    with pytest.raises(ValueError, match="available") as e:
        gridpin.sheets.resolve("atlantis")
    assert first["code"] in str(e.value)


def test_the_cache_folder_follows_gridpin_home_with_the_release_inside(home):
    sheet = gridpin.sheets.list()[-1]
    assert gridpin.sheets.home() == home
    assert gridpin.sheets.path(sheet.code) == home / REGISTRY["release"] / sheet.file
    assert sheet.path == home / REGISTRY["release"] / sheet.file
    assert not gridpin.sheets.is_downloaded(sheet.code) and not sheet.downloaded
    assert gridpin.sheets.verify(sheet.code) == "missing"
    assert gridpin.sheets.verify(sheet.code, file=SHEET) == "wrong_size", "file= checks that file, not the cache"


def test_downloaded_means_the_pinned_size_and_verify_reads_the_digest(home):
    sheet = gridpin.sheets.list()[-1]
    place = gridpin.sheets.path(sheet.code)
    place.parent.mkdir(parents=True)
    with open(place, "wb") as f:  # sparse file of the pinned size: "downloaded", but not the release bytes
        f.truncate(sheet.size)
    assert gridpin.sheets.is_downloaded(sheet.code)
    assert gridpin.sheets.verify(sheet.code) == "wrong_sha256"
    with open(place, "ab") as f:
        f.write(b"\0")
    assert not gridpin.sheets.is_downloaded(sheet.code)
    assert gridpin.sheets.verify(sheet.code) == "wrong_size"
    assert gridpin.sheets.verify(sheet.code, file=SHEET) == "wrong_size"


def test_a_released_sheet_verifies_ok_where_one_is_on_disk(home):
    sheet = gridpin.sheets.list()[-1]
    released = [p for p in (CODE / "data").glob(f'public-*-{REGISTRY["release"]}/{sheet.file}')]
    if not released:
        pytest.skip(f"no {sheet.file} of {REGISTRY['release']} under data/")
    place = gridpin.sheets.path(sheet.code)
    place.parent.mkdir(parents=True)
    place.symlink_to(released[0])  # a symbolic link: the released file keeps exactly one name
    assert gridpin.sheets.verify(sheet.code) == "ok"
    assert gridpin.sheets.fetch(sheet.code, offline=True) == place


def test_fetch_offline_without_the_sheet_names_the_url_and_keeps_nothing(home, monkeypatch):
    # A broken offline switch must not reach the network from a test: "curl" here always fails.
    monkeypatch.setenv("GRIDPIN_CURL", "false")
    sheet = gridpin.sheets.list()[-1]
    with pytest.raises(OSError) as e:
        gridpin.sheets.fetch(sheet.code, offline=True)
    assert sheet.url in str(e.value) and "offline" in str(e.value)
    monkeypatch.setenv("GRIDPIN_OFFLINE", "1")
    with pytest.raises(OSError, match="offline"):
        gridpin.sheets.fetch(sheet.code)
    assert not any(home.rglob("*.bin"))


def test_a_download_that_does_not_match_the_release_is_discarded(home, tmp_path_factory, monkeypatch):
    """GRIDPIN_CURL names the program the engine runs instead of curl; this one writes wrong bytes."""
    fake = tmp_path_factory.mktemp("curl") / "curl"
    fake.write_text('#!/bin/sh\nwhile [ "$1" != "-o" ]; do shift; done\nprintf wrong > "$2"\n')
    fake.chmod(0o755)
    monkeypatch.setenv("GRIDPIN_CURL", str(fake))
    sheet = gridpin.sheets.list()[-1]
    with pytest.raises(OSError, match="expected"):
        gridpin.sheets.fetch(sheet.code)
    assert not gridpin.sheets.path(sheet.code).exists()
    assert [p.name for p in gridpin.sheets.path(sheet.code).parent.iterdir()] == []


def test_open_path_takes_a_file_as_is_and_a_country_only_when_downloaded(home):
    assert gridpin.sheets.open_path(SHEET) == SHEET
    assert gridpin.sheets.open_path(str(SHEET)) == SHEET
    code = gridpin.sheets.list()[-1].code
    with pytest.raises(FileNotFoundError, match=rf"gridpin\.sheets\.fetch\('{code}'\)"):
        gridpin.sheets.open_path(code)


# ---- gridpin.pandas ----------------------------------------------------------------------------------------------

@pytest.fixture(scope="module")
def pd():
    return pytest.importorskip("pandas")


@pytest.fixture(scope="module")
def gp(pd, geo):
    import gridpin.pandas as gp_pandas

    return gp_pandas


def test_answer_columns_are_those_of_the_cli_csv(gp):
    source = (CRATE / "src" / "tabular.rs").read_text(encoding="utf-8")
    block = source[source.index("pub const GP_COLUMNS"):]
    cli = re.findall(r'"(gp_[a-z_]+)"', block[:block.index("];")])
    assert list(gp.GP_COLUMNS) == cli


def test_geocode_df_keeps_every_row_in_order_with_the_engine_answer(pd, gp, geo):
    df = pd.DataFrame({"address": [NOWHERE, ADDRESS, None, "", float("nan")], "id": [5, 4, 3, 2, 1]},
                      index=["e", "d", "c", "b", "a"])
    out = gp.geocode_df(df, "address", SHEET)
    assert list(out.index) == list(df.index) and list(out["id"]) == [5, 4, 3, 2, 1]
    assert list(out.columns) == ["address", "id", *gp.GP_COLUMNS]
    top = geo.geocode(ADDRESS, 1)[0]
    row = out.loc["d"]
    assert (row.gp_lat, row.gp_lon) == (top["lat"], top["lon"])
    assert (row.gp_street, row.gp_housenumber, row.gp_commune, row.gp_postcode) == (
        top["street"], top["housenumber"], top["commune"], top["postcode"])
    assert (row.gp_precision, row.gp_confidence, bool(row.gp_weak)) == (top["precision"], top["confidence"], top["weak"])
    assert (row.gp_match_level, row.gp_match_status) == (top["match_level"], top["match_status"])
    for label in "ecba":
        assert pd.isna(out.loc[label, "gp_lat"]) and pd.isna(out.loc[label, "gp_weak"])
        assert out.loc[label, "gp_match_status"] == "U" and pd.isna(out.loc[label, "gp_match_level"])
    assert "address" in df.columns and "gp_lat" not in df.columns, "the input frame is not changed"


ALL_EXTRAS = "h3,geohash,placekey,codes,street_point,links,provenance"


def test_extras_columns_are_those_of_the_cli_csv_with_the_cli_values(pd, gp, geo, tmp_path):
    cli = CRATE / "target" / "release" / "gridpin"
    if not cli.exists():
        pytest.skip("CLI not built (make engine)")
    src = tmp_path / "in.csv"
    pd.DataFrame({"address": [ADDRESS, NOWHERE]}).to_csv(src, index=False)
    out = tmp_path / "out.csv"
    subprocess.run([str(cli), "batch", str(SHEET), str(src), str(out), "--with", ALL_EXTRAS], check=True,
                   capture_output=True)
    theirs = pd.read_csv(out, dtype=str, keep_default_na=False)
    ours = gp.geocode_df(pd.DataFrame({"address": [ADDRESS, NOWHERE]}), "address", SHEET, extras=ALL_EXTRAS)
    assert list(ours.columns) == list(theirs.columns)
    extra = [c for c in theirs.columns if c not in ("address", *gp.GP_COLUMNS)]
    assert extra and extra[0] == "gp_h3"
    for col in extra:
        got = ["" if pd.isna(v) else (format(v, "g") if isinstance(v, float) else str(v)) for v in ours[col]]
        assert got == list(theirs[col]), col
    assert "gp_h3" not in gp.geocode_df(pd.DataFrame({"address": [ADDRESS]}), "address", SHEET).columns
    with pytest.raises(ValueError, match="unknown field"):
        gp.geocode_df(pd.DataFrame({"address": [ADDRESS]}), "address", SHEET, extras="zip")


def test_the_engine_adds_extras_only_when_asked(geo):
    assert "extras" not in geo.geocode(ADDRESS, 1)[0]
    top = geo.geocode(ADDRESS, 1, extras="h3:8,placekey")[0]
    assert set(top["extras"]) == {"h3", "placekey_where"} and top["extras"]["h3"].startswith("88")
    assert geo.geocode_many([ADDRESS], 1, extras="geohash:5")[0][0]["extras"]["geohash"].__len__() == 5
    back = geo.reverse(top["lat"], top["lon"], 1, extras="codes")[0]
    assert {"plus_code", "mgrs", "dms"} == set(back["extras"])


def test_geocode_or_ask_gives_the_answer_of_geocode_and_says_its_kind(geo):
    out = geo.geocode_or_ask(ADDRESS, 1)
    assert out["kind"] == "answer" and out["results"] == geo.geocode(ADDRESS, 1)
    silent = geo.geocode_or_ask(NOWHERE, 1)
    assert silent["kind"] == "none"
    assert silent["reason"] in ("no_street", "no_house", "city_unknown", "outside_sheet", "unparsed")


def test_k_gives_one_row_per_candidate_numbered_by_gp_rank(pd, gp, geo):
    df = pd.DataFrame({"address": [SEVERAL, NOWHERE]})
    out = gp.geocode_df(df, "address", SHEET, k=3)
    hits = geo.geocode(SEVERAL, 3)
    assert len(hits) > 1
    assert list(out["gp_rank"].iloc[:len(hits)]) == list(range(1, len(hits) + 1))
    assert list(out["gp_lat"].iloc[:len(hits)]) == [h["lat"] for h in hits]
    assert len(out) == len(hits) + 1 and pd.isna(out["gp_rank"].iloc[-1])
    assert "gp_rank" not in gp.geocode_df(df, "address", SHEET, k=1).columns


def test_a_mapping_sends_the_cells_as_fields(pd, gp, geo):
    df = pd.DataFrame({"s": ["boulevard des moulins", "27 boulevard des moulins monaco", None, None],
                       "n": ["27", None, None, None], "c": ["monaco", None, "monaco", None]})
    out = gp.geocode_df(df, {"street": "s", "number": "n", "city": "c"}, SHEET)
    top = geo.geocode("boulevard des moulins 27 monaco", 1)[0]
    assert (out.gp_street[0], out.gp_housenumber[0]) == (top["street"], top["housenumber"])
    # a street cell the field reading cannot use falls to the line; a blank street asks the other cells
    assert out.gp_lat[1] == geo.geocode("27 boulevard des moulins monaco", 1)[0]["lat"]
    assert out.gp_precision[2] == geo.geocode("monaco", 1)[0]["precision"]
    assert pd.isna(out.gp_lat[3]) and out.gp_match_status[3] == "U"
    with pytest.raises(ValueError, match="street"):
        gp.geocode_df(df, {"city": "c"}, SHEET)
    with pytest.raises(ValueError, match="unknown"):
        gp.geocode_df(df, {"street": "s", "zip": "c"}, SHEET)
    with pytest.raises(KeyError):
        gp.geocode_df(df, {"street": "nope"}, SHEET)


def test_the_faker_provider_gives_real_addresses_of_the_sheet(geo):
    from gridpin.faker import GridPinProvider

    fake = GridPinProvider(str(SHEET), n=50, seed=3)
    row = fake.real_address_row()
    assert any(h["lat"] == row["lat"] and h["lon"] == row["lon"]
               for h in geo.houses(street=row["street"], city=row["commune"]))
    assert all(fake.street_address() and fake.city() and fake.address() for _ in range(20))
    lat, lon = fake.latlng()
    assert isinstance(lat, float) and isinstance(lon, float)
    # the same seed, the same addresses in the same order
    again = GridPinProvider(str(SHEET), n=50, seed=3)
    first = GridPinProvider(str(SHEET), n=50, seed=3)
    assert [first.address() for _ in range(10)] == [again.address() for _ in range(10)]
    assert GridPinProvider(str(SHEET), n=5, seed=3, city="monaco").real_address_row()["commune"]
    with pytest.raises(ValueError):
        GridPinProvider(str(SHEET), n=5, seed=3, bbox=(0.0, 0.0, 0.1, 0.1))


def test_faker_takes_the_provider_by_its_public_methods():
    # what Faker's add_provider does with an instance: every public callable becomes a formatter
    from gridpin.faker import GridPinProvider

    names = [n for n in dir(GridPinProvider(str(SHEET), n=3, seed=1)) if not n.startswith("_")]
    assert {"address", "street_address", "city", "postcode", "latlng"} <= set(names)
    assert all(callable(getattr(GridPinProvider, n)) for n in names)


def test_the_batch_function_of_a_data_platform_keeps_every_row(pd, geo):
    from gridpin.pandas import GP_COLUMNS
    from gridpin.udf import SPARK_SCHEMA, geocode_iter

    batches = [pd.Series([ADDRESS, None, "", NOWHERE]), pd.Series([ADDRESS]), pd.Series([], dtype="object")]
    out = list(geocode_iter(str(SHEET))(iter(batches)))
    assert [len(f) for f in out] == [4, 1, 0], "a batch keeps its length"
    assert list(out[0].columns) == list(GP_COLUMNS)
    top = geo.geocode(ADDRESS, 1)[0]
    assert out[0].gp_lat[0] == top["lat"] and out[1].gp_street[0] == top["street"]
    assert list(out[0].gp_match_status[1:]) == ["U", "U", "U"]
    assert out[0].gp_lat[1:].isna().all()
    assert [c.split()[0] for c in SPARK_SCHEMA.split(", ")] == list(GP_COLUMNS)
    geocode_iter("/no/such/sheet.bin")  # nothing opens until the first batch reaches a worker
    assert "gp_weak boolean" in SPARK_SCHEMA and "gp_lat double" in SPARK_SCHEMA


def test_a_missing_cell_is_not_sent_to_the_engine_as_text(pd, monkeypatch):
    import gridpin.udf as udf

    asked = []

    class Spy:
        def geocode_many(self, queries, k):
            asked.extend(queries)
            return [[] for _ in queries]

    monkeypatch.setattr(udf, "geocoder", lambda sheet, poi: Spy())
    list(udf.geocode_iter("any.bin")(iter([pd.Series([ADDRESS, None, float("nan"), "  "])])))
    assert asked == [ADDRESS]


def test_several_columns_join_like_the_cli(pd, gp, geo):
    df = pd.DataFrame({"street": ["boulevard des moulins 27", "boulevard des moulins 27"],
                       "city": ["monaco", None]})
    out = gp.geocode_df(df, ["street", "city"], SHEET)
    joined = geo.geocode("boulevard des moulins 27, monaco", 1)[0]
    alone = geo.geocode("boulevard des moulins 27", 1)[0]
    assert (out["gp_lat"].iloc[0], out["gp_lon"].iloc[0]) == (joined["lat"], joined["lon"])
    assert (out["gp_lat"].iloc[1], out["gp_lon"].iloc[1]) == (alone["lat"], alone["lon"])
    # The smoke sheet answers the same with any separator, so the query text itself is checked too.
    df["postcode"] = ["  98000 ", float("nan")]
    assert gp._queries(df, ["street", "city", "postcode"]) == [
        "boulevard des moulins 27, monaco, 98000", "boulevard des moulins 27"]


def test_the_accessor_is_the_same_call(pd, gp):
    df = pd.DataFrame({"address": [ADDRESS, NOWHERE]})
    pd.testing.assert_frame_equal(df.gridpin.geocode("address", SHEET), gp.geocode_df(df, "address", SHEET))
    points = pd.DataFrame({"y": [43.7396], "x": [7.4265]})
    pd.testing.assert_frame_equal(points.gridpin.reverse(SHEET, lat="y", lon="x"),
                                  gp.reverse_df(points, SHEET, lat="y", lon="x"))


def test_reverse_df_gives_the_nearest_house_and_its_distance(pd, gp, geo):
    df = pd.DataFrame({"lat": [43.7396, None, 43.7437], "lon": [7.4265, 7.4, 7.4285]})
    out = gp.reverse_df(df, SHEET)
    assert list(out.columns) == ["lat", "lon", *gp.GP_COLUMNS, "gp_distance_m"]
    for i in (0, 2):
        near = geo.reverse(df["lat"][i], df["lon"][i], 1)[0]
        assert (out["gp_lat"][i], out["gp_lon"][i], out["gp_distance_m"][i]) == (
            near["lat"], near["lon"], near["distance_m"])
    assert pd.isna(out["gp_lat"][1]) and pd.isna(out["gp_distance_m"][1])
    ranked = gp.reverse_df(df, SHEET, k=2)
    assert list(ranked["gp_rank"].dropna()) == [1, 2, 1, 2]


def test_reverse_df_names_the_row_with_an_impossible_coordinate(pd, gp):
    df = pd.DataFrame({"lat": [43.7396, 123.0], "lon": [7.4265, 7.4]}, index=["ok", "bad"])
    with pytest.raises(ValueError, match="row 'bad'"):
        gp.reverse_df(df, SHEET)


def test_wrong_inputs_say_what_is_wrong(pd, gp, geo):
    df = pd.DataFrame({"address": [ADDRESS]})
    with pytest.raises(KeyError, match="nope"):
        gp.geocode_df(df, "nope", SHEET)
    for k in (0, 1.5, True, "2"):
        with pytest.raises(ValueError, match="k must"):
            gp.geocode_df(df, "address", SHEET, k=k)
    with pytest.raises(ValueError, match="already has answer columns"):
        gp.geocode_df(gp.geocode_df(df, "address", SHEET), "address", SHEET)
    with pytest.raises(ValueError, match="poi"):
        gp.geocode_df(df, "address", geo, poi=SHEET)


def test_an_open_geocoder_and_a_country_code_are_accepted_as_the_sheet(pd, gp, geo, home):
    df = pd.DataFrame({"address": [ADDRESS]})
    pd.testing.assert_frame_equal(gp.geocode_df(df, "address", geo), gp.geocode_df(df, "address", SHEET))
    code = gridpin.sheets.list()[-1].code
    with pytest.raises(FileNotFoundError, match="gridpin.sheets.fetch"):
        gp.geocode_df(df, "address", code)


# ---- gridpin.geopy -----------------------------------------------------------------------------------------------

@pytest.fixture(scope="module")
def GridPin(geo):
    pytest.importorskip("geopy")
    from gridpin.geopy import GridPin as cls

    return cls


def test_geopy_geocode_returns_a_location_with_the_engine_answer(GridPin, geo):
    place = GridPin(sheet=SHEET).geocode(ADDRESS)
    top = geo.geocode(ADDRESS, 1)[0]
    assert (place.latitude, place.longitude) == (top["lat"], top["lon"])
    assert place.address == f'{top["housenumber"]} {top["street"]}, {top["commune"]}, {top["postcode"]}'
    assert place.raw == top


def test_geopy_lists_reverse_and_none(GridPin, geo):
    coder = GridPin(sheet=SHEET)
    many = coder.geocode(SEVERAL, exactly_one=False, limit=3)
    assert len(many) == 3
    assert [(p.latitude, p.longitude) for p in many] == [(h["lat"], h["lon"]) for h in geo.geocode(SEVERAL, 3)]
    near = coder.reverse("43.7396, 7.4265")
    first = geo.reverse(43.7396, 7.4265, 1)[0]
    assert (near.latitude, near.longitude) == (first["lat"], first["lon"])
    assert coder.geocode(NOWHERE) is None


def test_geopy_needs_a_country_or_a_sheet(GridPin, home):
    with pytest.raises(ValueError, match="country"):
        GridPin()
    with pytest.raises(FileNotFoundError, match="gridpin.sheets.fetch"):
        GridPin(gridpin.sheets.list()[-1].code)
