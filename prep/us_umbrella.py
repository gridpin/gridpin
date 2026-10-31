"""GeoNames umbrella locality for US address rows whose source carries no city at all.

Up to about 5 % of US source rows have an empty city (Texas 1.7 M, New York outside the
city 0.97 M, Oregon 0.25 M, Colorado 0.24 M by the 2026-09-23.1 field survey). The umbrella
assigns each such row the nearest populated place of the SAME state from the GeoNames dump
(CC BY 4.0, permissive, not OSM), so that a query naming the town still reaches the house.

The mechanism mirrors the Serbian umbrella in prep/overture.py (u1): a 0.1-degree grid,
candidates within +-3 cells, and the nearest place by squared equirectangular distance
divided by a population weight 1 + ln(1 + pop) -- a large city wins its own core over
population-less sub-centres, while standalone towns keep their own name. Two differences:

* County seats alone (PPLA2, 2 988) are too sparse for the US, so the place table holds every
  class-P feature with population >= 1 000 (16 950) plus PPLC/PPLA/PPLA2/PPLA3 without a
  population threshold.
* The state is a hard constraint first: the nearest place of the row's own state wins even
  when a bigger place across the border is closer. Only when no place of the same state lies
  within +-3 cells does the row take the nearest place of any state, flagged `cross_state`.

Memory: every step is its own CREATE TABLE, and nothing is computed over the whole 100-million
row sheet -- the umbrella sees only the rows without a city (`attach_umbrella` extracts them
first). The grid join is an equi-join on expanded place cells (each place is listed under the
49 cells it can serve), and the nearest place is an `arg_min` aggregate rather than a window,
so the working set is one struct per pending row.
"""

CENTRE_CODES = ("PPLC", "PPLA", "PPLA2", "PPLA3")
MIN_POPULATION = 1000
CELL_REACH = 3  # 0.1-degree cells on each side, as the Serbian umbrella

# Squared equirectangular distance in degrees, scaled by the population weight (u1 of Serbia).
_WEIGHTED_DIST2 = (
    "((a.lat-p.lat)*(a.lat-p.lat)"
    "+((a.lon-p.lon)*cos(radians(a.lat)))*((a.lon-p.lon)*cos(radians(a.lat))))"
    "/(1+ln(1+p.pop))"
)
# arg_min needs a total order: weighted distance, then the larger population, then the name.
_BEST = (
    "arg_min(struct_pack(name := p.name, admin1 := p.admin1),"
    f" struct_pack(w := {_WEIGHTED_DIST2}, pop := -p.pop, name := p.name))"
)
_CELL_JOIN = (
    "p.cla = CAST(round(a.lat*10) AS BIGINT) AND p.clo = CAST(round(a.lon*10) AS BIGINT)"
)


def _sql_literal(text):
    return str(text).replace("'", "''")


def places_sql(geonames_path, table="places", admin1_map=None):
    """SQL creating `table` from a GeoNames country dump (tab-separated, 19 columns).

    Columns: name, admin1 (state code), lat, lon, pop, fcode, bla, blo (0.1-degree cells).
    Selection: class P with population >= MIN_POPULATION, or an administrative seat of
    CENTRE_CODES regardless of population. `admin1_map` translates the dump's admin1 codes to
    the sheet's region codes (Canada: "08" -> "ON"); a code missing from the map stays as it is.
    """
    codes = ", ".join(f"'{c}'" for c in CENTRE_CODES)
    admin1 = "column10"
    if admin1_map:
        whens = " ".join(f"WHEN '{_sql_literal(k)}' THEN '{_sql_literal(v)}'"
                         for k, v in sorted(admin1_map.items()))
        admin1 = f"CASE column10 {whens} ELSE column10 END"
    return f"""CREATE OR REPLACE TABLE {table} AS
SELECT column01 AS name, {admin1} AS admin1,
       CAST(column04 AS DOUBLE) AS lat, CAST(column05 AS DOUBLE) AS lon,
       greatest(coalesce(TRY_CAST(column14 AS BIGINT), 0), 0) AS pop,
       column07 AS fcode,
       CAST(round(CAST(column04 AS DOUBLE)*10) AS BIGINT) AS bla,
       CAST(round(CAST(column05 AS DOUBLE)*10) AS BIGINT) AS blo
FROM read_csv('{_sql_literal(geonames_path)}', delim='\t', header=false, quote='', all_varchar=true)
WHERE column06 = 'P'
  AND (coalesce(TRY_CAST(column14 AS BIGINT), 0) >= {MIN_POPULATION} OR column07 IN ({codes}))"""


def umbrella_sql(addr_table, places_table, out_table="umbrella"):
    """SQL steps assigning an umbrella place to every row of `addr_table` (rid, lat, lon, state).

    Every row of `addr_table` is treated as pending, i.e. without a city -- the caller selects
    those rows. The result `out_table` has rid, name, admin1, cross_state (BOOLEAN); rows with
    no place within reach in any state are absent from it. Statements are meant to run one by
    one so that each step's working memory is released before the next.
    """
    reach = CELL_REACH
    cells = f"{out_table}_cells"
    same, cross = f"{out_table}_same", f"{out_table}_cross"
    return [
        # Each place is listed under every cell within reach, turning the +-N-cell window into
        # an equi-join: 17 k places x 49 cells is small, the address side stays unexpanded.
        f"""CREATE OR REPLACE TABLE {cells} AS
SELECT p.name, p.admin1, p.lat, p.lon, p.pop, p.bla + dla.x AS cla, p.blo + dlo.x AS clo
FROM {places_table} p, range(-{reach}, {reach + 1}) dla(x), range(-{reach}, {reach + 1}) dlo(x)""",
        # Same-state umbrella: the population-weighted nearest place of the row's own state.
        f"""CREATE OR REPLACE TABLE {same} AS
SELECT a.rid, {_BEST} AS best
FROM {addr_table} a JOIN {cells} p ON {_CELL_JOIN} AND p.admin1 = a.state
GROUP BY a.rid""",
        # Cross-state fallback for rows the same-state step left out: any state, same measure.
        f"""CREATE OR REPLACE TABLE {cross} AS
SELECT a.rid, {_BEST} AS best
FROM {addr_table} a ANTI JOIN {same} s ON s.rid = a.rid
JOIN {cells} p ON {_CELL_JOIN}
GROUP BY a.rid""",
        f"""CREATE OR REPLACE TABLE {out_table} AS
SELECT rid, best.name AS name, best.admin1 AS admin1, false AS cross_state FROM {same}
UNION ALL
SELECT rid, best.name AS name, best.admin1 AS admin1, true AS cross_state FROM {cross}""",
        f"DROP TABLE {cells}",
        f"DROP TABLE {same}",
        f"DROP TABLE {cross}",
    ]


def attach_umbrella(con, addr_table, geonames_path, city_col="city", rid_col="rid",
                    out_table="umbrella", admin1_map=None):
    """Fill the empty `city_col` of `addr_table` with the umbrella place; return (filled, cross).

    `addr_table` needs `rid_col` (unique; `rowid` of a base table works), lat, lon, state and
    `city_col`. Rows whose city is already set are not touched and not counted. The result
    table `out_table` (rid, name, admin1, cross_state) is left in place for inspection; the
    place table and the pending-row table are dropped.
    """
    places, pending = f"{out_table}_places", f"{out_table}_pending"
    con.execute(places_sql(geonames_path, places, admin1_map))
    con.execute(f"""CREATE OR REPLACE TABLE {pending} AS
SELECT {rid_col} AS rid, lat, lon, state FROM {addr_table} WHERE coalesce({city_col}, '') = ''""")
    for statement in umbrella_sql(pending, places, out_table):
        con.execute(statement)
    con.execute(f"""UPDATE {addr_table} SET {city_col} = u.name
FROM {out_table} u WHERE {addr_table}.{rid_col} = u.rid""")
    filled, cross = con.execute(
        f"SELECT count(*), count(*) FILTER (WHERE cross_state) FROM {out_table}").fetchone()
    con.execute(f"DROP TABLE {pending}")
    con.execute(f"DROP TABLE {places}")
    return int(filled), int(cross)
