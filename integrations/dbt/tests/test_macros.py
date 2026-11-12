"""The dbt macros render to the DuckDB and Postgres SQL that GridPin's functions take (rendered with jinja2, no dbt)."""
from pathlib import Path

import pytest

MACROS = (Path(__file__).resolve().parents[1] / "macros" / "gridpin.sql").read_text()


MODEL = (Path(__file__).resolve().parents[1] / "models" / "example_geocoded.sql").read_text()


def _package(target):
    """The macros as dbt calls them: dispatched on the target and reachable as `gridpin.<macro>` from inside."""
    jinja2 = pytest.importorskip("jinja2")
    env = jinja2.Environment()

    class Adapter:
        def dispatch(self, name, package):
            return getattr(inner, f"{target}__{name}")

    ctx = {"adapter": Adapter(), "return": lambda x: x}
    inner = env.from_string(MACROS).make_module(ctx)
    outer = env.from_string(MACROS).make_module({**ctx, "gridpin": inner})
    return env, ctx, outer


def render(target, expr):
    env, ctx, pkg = _package(target)
    return " ".join(env.from_string("{{ " + expr + " }}").render(**ctx, gridpin=pkg).split())


def render_model(target, customers):
    env, ctx, pkg = _package(target)
    return env.from_string(MODEL).render(**ctx, gridpin=pkg, ref=lambda name: customers)


def test_duckdb_and_postgres_forms():
    assert render("duckdb", "gridpin.gridpin_geocode('address')") == "gridpin_geocode(address)"
    assert render("postgres", "gridpin.gridpin_geocode('address')") == "gridpin_geocode(address)::text"


def test_postgres_reads_the_fields_with_jsonb_operators():
    """Revision 0.9.0: on Postgres the column macros gave `json_extract_string(...)` and `as double`, and both fail
    there ("function json_extract_string(text, unknown) does not exist", "type \"double\" does not exist")."""
    assert render("postgres", "gridpin.gridpin_lat('address')") == \
        "cast(((gridpin_geocode(address)::text)::jsonb ->> 'lat') as double precision)"
    assert render("postgres", "gridpin.gridpin_level('address')") == \
        "((gridpin_geocode(address)::text)::jsonb ->> 'match_level')"
    for name in ("gridpin_lat", "gridpin_lon", "gridpin_level"):
        sql = render("postgres", f"gridpin.{name}('address')")
        assert "json_extract_string" not in sql and " as double)" not in sql


STAND_IN = {"10 rue de Rivoli, Paris": '{"lat":48.85,"lon":2.35,"match_level":"PointAddress","weak":false}',
           "Paris": '{"lat":48.86,"lon":2.34,"match_level":"Locality","weak":true}',
           "zzqx": "{}"}


def test_the_duckdb_forms_and_the_example_model_run_in_duckdb():
    """The rendered SQL runs in DuckDB against a stand-in gridpin_geocode with the extension's answer shape."""
    duckdb = pytest.importorskip("duckdb")
    con = duckdb.connect()
    cases = " ".join(f"WHEN a = '{q}' THEN '{j}'" for q, j in STAND_IN.items())
    con.execute(f"CREATE MACRO gridpin_geocode(a) AS (CASE {cases} END)")
    con.execute("CREATE TABLE customers(id INT, address TEXT)")
    con.executemany("INSERT INTO customers VALUES (?, ?)", list(enumerate(STAND_IN)))
    lat, lon, level = (render("duckdb", f"gridpin.{m}('address')") for m in ("gridpin_lat", "gridpin_lon", "gridpin_level"))
    rows = con.execute(f"SELECT {lat}, {lon}, {level} FROM customers ORDER BY id").fetchall()
    assert rows == [(48.85, 2.35, "PointAddress"), (48.86, 2.34, "Locality"), (None, None, None)]
    model = con.execute(render_model("duckdb", "customers") + " ORDER BY id").fetchall()
    assert [r[2:] for r in model] == [(48.85, 2.35, "PointAddress", False), (48.86, 2.34, "Locality", True),
                                      (None, None, None, None)]


def test_the_example_model_geocodes_each_address_once():
    """The documentation example called the geocoder three times a row (x4.3 time on 100 rows, revision 0.9.0)."""
    for target in ("duckdb", "postgres"):
        model = render_model(target, "customers")
        # an inlined CTE puts the call back into every field that reads it (SQLite 3.41: 4 calls a row)
        assert model.count("gridpin_geocode(") == 1 and "as materialized (" in model


def test_the_column_macros_call_the_geocoder_through_the_package_name():
    """A project that installs the package calls `gridpin.gridpin_lat(...)`; inside it a bare `gridpin_geocode` is not
    in scope, and dbt stops with "'gridpin_geocode' is undefined" (a real dbt-duckdb 1.11.1 run, 10.11.2026). The
    jinja2 rendering above puts every macro in one scope and cannot see this, so the text is checked."""
    import re
    for name in ("gridpin_lat", "gridpin_lon", "gridpin_level"):
        body = re.search(r"{% macro " + name + r"\(column\) -%}(.*?){%- endmacro %}", MACROS, re.S).group(1)
        calls = re.findall(r"(\w+\.)?gridpin_geocode\(", body)
        assert calls and all(c == "gridpin." for c in calls), (name, calls)
