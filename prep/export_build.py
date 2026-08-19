#!/usr/bin/env python3
"""Export the input stream for the index builder: CSV sorted by (street, commune).

The sort order matches the lexicographic order of FST keys
(`nom_voie_norm + 0x1F + code_insee`), so the builder runs in a single pass.

Usage: python3 prep/export_build.py [input.parquet] [output.csv.gz]
Defaults: data/ban_norm.parquet -> data/build_input.csv.gz
"""
import pathlib
import sys
import time

import duckdb  # noqa: F401 (typing)
from duck import connect_tuned

CODE = pathlib.Path(__file__).resolve().parent.parent
SRC = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else CODE / "data" / "ban_norm.parquet"
OUT = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else CODE / "data" / "build_input.csv.gz"

t0 = time.time()
compression = ", COMPRESSION gzip" if str(OUT).endswith(".gz") else ""
con = connect_tuned()
cols = {r[0] for r in con.execute(f"DESCRIBE SELECT * FROM '{SRC}'").fetchall()}
prov = "provincia_norm" if "provincia_norm" in cols else "''"
# full postcode string (e.g. NL "1012XJ"); fall back to numeric code_postal when absent
cpd = "code_postal_display" if "code_postal_display" in cols else "code_postal"
con.execute(f"""
    COPY (
        SELECT trim(regexp_replace(nom_voie_norm, '\\s+', ' ', 'g')) AS nom_voie_norm, code_insee,
               trim(regexp_replace(nom_commune_norm, '\\s+', ' ', 'g')) AS nom_commune_norm, code_postal,
               {cpd} AS code_postal_display,
               numero,
               -- canonical suffix form: no spaces/hyphens/dots ("a-429" -> "a429")
               coalesce(lower(regexp_replace(rep, '[\\s\\-/\\.ʻʼ‘]', '', 'g')), '') AS rep,
               lon, lat, nom_voie, nom_commune,
               {prov} AS provincia_norm
        FROM '{SRC}'
        -- full ordering (voie, insee, numero, rep, lon, lat) for determinism: duplicate
        -- rows for the same house number (merged sources) would otherwise arrive in
        -- arbitrary order and find_house could pick a different one on every rebuild
        -- (496 French groups differ in lat alone, so lat must take part). The rep
        -- expression is repeated verbatim instead of ordinal 7: inserting a column
        -- would silently shift positional sort keys, and a bare
        -- `rep` here would be ambiguous against the source column.
        ORDER BY nom_voie_norm, code_insee, numero,
                 coalesce(lower(regexp_replace(rep, '[\\s\\-/\\.ʻʼ‘]', '', 'g')), ''), lon, lat
    ) TO '{OUT}' (FORMAT csv, HEADER true{compression})
""")
print(f"{OUT} ready: {OUT.stat().st_size/1e6:.0f} MB in {time.time()-t0:.0f} s")
