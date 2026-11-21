"""A polygon layer for `gridpin-pro batch --append regions:<layer>`: any polygon source to the WOFA format.

    python3 prep/regions_layer.py OUT.bin SOURCE --name "<SQL expression>" [--where "<SQL condition>"]

SOURCE: GeoJSON, a Shapefile (also inside a .zip: `/vsizip/path.zip/file.shp`) or GeoParquet / a parquet with a
WKB column `wkb`. `--name` is a DuckDB expression over the source's columns that names each polygon (the value the
layer answers, at most 255 bytes: `NUTS_ID`, `SA2_CODE26 || '|' || SA2_NAME26`); `--where` keeps some rows.
`--from-crs EPSG:3347` reprojects a source that is not in degrees (StatCan boundaries are in Statistics Canada
Lambert) to WGS 84 longitude, latitude.
`--kind us_tract --vintage 2020 --grain fine --layer-source … --layer-licence …` print the layer's passport for `layers.json`
(plan T1.0): the kind of code its names carry, the year the boundaries were drawn, whether the areas are small.
Geometry is read through DuckDB `spatial`; rings and the file format are those of `prep/us_census.py` (the census
layers), a ring longer than the format's u16 is simplified until it fits.
"""
import argparse
import json
import pathlib
import struct
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from duck import connect_tuned  # noqa: E402
from us_census import MAX_RING_POINTS, encode, rings_of  # noqa: E402


def geometry_sql(source):
    if source.endswith(".parquet"):
        return f"SELECT *, ST_GeomFromWKB(wkb) AS geom FROM read_parquet('{source}')"
    return f"SELECT * FROM ST_Read('{source}')"


def build(out, source, name, where=None, simplify=0.00005, from_crs=None):
    con = connect_tuned()
    con.execute("INSTALL spatial; LOAD spatial;")
    base = geometry_sql(source)
    cond = f"WHERE {where}" if where else ""
    geom = f"ST_Transform(geom, '{from_crs}', 'EPSG:4326', always_xy := true)" if from_crs else "geom"
    rows = con.execute(
        f"SELECT ({name})::VARCHAR AS n, ST_AsGeoJSON({geom}) AS g FROM ({base}) {cond}"
    ).fetchall()
    blobs, skipped = [], 0
    for n, g in rows:
        if not n or g is None:
            skipped += 1
            continue
        geometry = json.loads(g)
        if geometry.get("type") not in ("Polygon", "MultiPolygon"):
            skipped += 1
            continue
        rings = rings_of(geometry)
        if any(len(r) > MAX_RING_POINTS for r in rings):
            g2 = con.execute("SELECT ST_AsGeoJSON(ST_SimplifyPreserveTopology(ST_GeomFromGeoJSON(?), ?))",
                             [g, simplify * 20]).fetchone()[0]
            rings = [r for r in rings_of(json.loads(g2)) if len(r) <= MAX_RING_POINTS]
        blobs.append(encode(n.encode("utf-8")[:255].decode("utf-8", "ignore"), rings))
    out = pathlib.Path(out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with open(out, "wb") as f:
        f.write(b"WOFA" + struct.pack("<I", len(blobs)))
        for b in blobs:
            f.write(b)
    return len(blobs), skipped


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("out")
    p.add_argument("source")
    p.add_argument("--name", required=True)
    p.add_argument("--where")
    p.add_argument("--from-crs", help="the source's coordinate system when it is not degrees, e.g. EPSG:3347")
    p.add_argument("--kind", help="passport: the kind of code the names carry (us_tract, au_sa2, ...)")
    p.add_argument("--vintage", help="passport: the year the boundaries were drawn, not the year of the file")
    p.add_argument("--grain", choices=["fine", "coarse"], help="passport: fine (tract, small area) or coarse")
    # not --source: that is the name of the positional argument, and argparse would let the option overwrite it
    p.add_argument("--layer-source", help="passport: the source and its edition")
    p.add_argument("--layer-licence", help="passport: the licence of the source")
    a = p.parse_args(argv)
    n, skipped = build(a.out, a.source, a.name, a.where, from_crs=a.from_crs)
    print(f"{n} polygons -> {a.out} ({skipped} rows without a polygon or a name)", file=sys.stderr)
    if a.kind:
        print(json.dumps(passport(a, n)))


def passport(a, areas):
    """The entry of the layer in `layers.json`; `file` is the name to place beside the registry."""
    import datetime
    return {"file": pathlib.Path(a.out).name, "kind": a.kind, "vintage": a.vintage, "grain": a.grain,
            "source": a.layer_source, "licence": a.layer_licence, "built": datetime.date.today().isoformat(),
            "areas": areas}


if __name__ == "__main__":
    main()
