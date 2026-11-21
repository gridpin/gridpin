#!/usr/bin/env python3
"""Bavarian addresses from the LoD2 3D building models of the Bayerische Vermessungsverwaltung (CC BY 4.0).

Source: https://geodaten.bayern.de/opengeodata/ (product lod2, CityGML 1.0, ETRS89/UTM32, 2 x 2 km tiles listed in
09.meta4 with size and SHA-256). Licence: CC BY 4.0, attribution "Bayerische Vermessungsverwaltung –
www.geodaten.bayern.de" (ANSWERS §337, §339).

Tiles are never written to disk: each one is parsed from the network stream through a wrapper that counts bytes and
SHA-256, and its rows count only when both match the meta4 entry. Only buildings with an address are kept.

  python3 -I prep/de_lod2_bavaria.py tile <name|path> <meta4> <out_dir>   # one tile -> <out_dir>/<tile>.json + .csv
  python3 -I prep/de_lod2_bavaria.py fetch <meta4> <store_dir> [--pause S] # every tile, resumable
  python3 -I prep/de_lod2_bavaria.py manifest <base.json> <out.json>        # sheet passport with Bavaria
"""
from __future__ import annotations

import csv
import hashlib
import json
import re
import sys
import time
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

NS_CORE = "{http://www.opengis.net/citygml/1.0}"
NS_BLDG = "{http://www.opengis.net/citygml/building/1.0}"
NS_GML = "{http://www.opengis.net/gml}"
NS_GEN = "{http://www.opengis.net/citygml/generics/1.0}"
NS_XAL = "{urn:oasis:names:tc:ciq:xsdschema:xAL:2.0}"
MIRRORS = ("https://download1.bayernwolke.de/a/lod2/citygml/", "https://download2.bayernwolke.de/a/lod2/citygml/")
USER_AGENT = "geodata-research-client/0.1 (python-urllib)"


class TileError(Exception):
    pass


def load_meta4(path: str | Path) -> dict[str, tuple[int, str]]:
    text = Path(path).read_text()
    out = {}
    for name, body in re.findall(r'<file name="([^"]+)">(.*?)</file>', text, re.S):
        size = int(re.search(r"<size>(\d+)</size>", body).group(1))
        sha = re.search(r'<hash type="sha-256">([0-9a-f]+)</hash>', body).group(1)
        out[name] = (size, sha)
    return out


class HashingReader:
    """File-like wrapper: counts bytes and SHA-256 of everything read."""

    def __init__(self, raw):
        self.raw, self.size, self.h = raw, 0, hashlib.sha256()

    def read(self, n=-1):
        chunk = self.raw.read(n)
        self.size += len(chunk)
        self.h.update(chunk)
        return chunk


def _ring_xy(poslist: str) -> list[tuple[float, float]]:
    v = [float(t) for t in poslist.split()]
    return [(v[i], v[i + 1]) for i in range(0, len(v) - 2, 3)]


def _shoelace(pts):
    a = cx = cy = 0.0
    for (x0, y0), (x1, y1) in zip(pts, pts[1:] + pts[:1]):
        c = x0 * y1 - x1 * y0
        a += c
        cx += (x0 + x1) * c
        cy += (y0 + y1) * c
    a /= 2.0
    if abs(a) < 1e-9:
        return 0.0, None
    return abs(a), (cx / (6 * a), cy / (6 * a))


def building_record(b: ET.Element) -> dict:
    """One bldg:Building with its parts: function, AGS, addresses, ground-plan centroid in UTM32."""
    attrs = {}
    for sa in b.iter(NS_GEN + "stringAttribute"):
        val = sa.find(NS_GEN + "value")
        attrs.setdefault(sa.get("name"), val.text if val is not None else None)
    addresses = []
    for adr in b.iter(NS_BLDG + "address"):
        loc = adr.find(".//" + NS_XAL + "LocalityName")
        tf = adr.find(".//" + NS_XAL + "ThoroughfareName")
        extra = sorted({e.tag.replace(NS_XAL, "xAL:") for e in adr.iter() if e.tag.startswith(NS_XAL)}
                       - {"xAL:AddressDetails", "xAL:Country", "xAL:CountryName", "xAL:Locality",
                          "xAL:LocalityName", "xAL:Thoroughfare", "xAL:ThoroughfareName"})
        addresses.append({"locality": (loc.text or "").strip() if loc is not None else "",
                          "thoroughfare": (tf.text or "").strip() if tf is not None else "",
                          "extra_tags": extra})
    area_sum = wx = wy = 0.0
    xs, ys = [], []
    for gs in b.iter(NS_BLDG + "GroundSurface"):
        for pl in gs.iter(NS_GML + "posList"):
            pts = _ring_xy(pl.text or "")
            if len(pts) < 3:
                continue
            a, c = _shoelace(pts[:-1] if pts[0] == pts[-1] else pts)
            if c:
                area_sum += a
                wx += a * c[0]
                wy += a * c[1]
    fallback = False
    if area_sum > 0:
        x, y = wx / area_sum, wy / area_sum
    else:
        for pl in b.iter(NS_GML + "posList"):
            for px, py in _ring_xy(pl.text or ""):
                xs.append(px)
                ys.append(py)
        if not xs:
            x = y = None
        else:
            x, y, fallback = (min(xs) + max(xs)) / 2, (min(ys) + max(ys)) / 2, True
    fn = b.find(NS_BLDG + "function")
    ext = b.find(f"{NS_CORE}externalReference/{NS_CORE}externalObject/{NS_CORE}name")
    return {
        "gml_id": b.get(NS_GML + "id"),
        "alkis_id": ext.text if ext is not None else None,
        "function": fn.text if fn is not None else None,
        "ags": attrs.get("Gemeindeschluessel"),
        "plan_date": attrs.get("Grundrissaktualitaet"),
        "parts": sum(1 for _ in b.iter(NS_BLDG + "BuildingPart")),
        "addresses": addresses,
        "x": x, "y": y, "area": round(area_sum, 2), "centroid_fallback": fallback,
    }


def parse_tile(fileobj):
    """Yield one record per top-level building; memory stays bounded by clearing each member."""
    for _event, elem in ET.iterparse(fileobj, events=("end",)):
        if elem.tag == NS_CORE + "cityObjectMember":
            b = elem.find(NS_BLDG + "Building")
            if b is not None:
                yield building_record(b)
            elem.clear()


def open_tile(name: str, attempt: int = 0):
    src = name if "/" in name and Path(name).exists() else None
    if src:
        return open(src, "rb"), None
    url = MIRRORS[attempt % 2] + name
    resp = urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": USER_AGENT}), timeout=60)
    ctype = resp.headers.get("Content-Type", "")
    if "html" in ctype:
        raise TileError(f"{url}: HTML instead of a tile ({ctype})")
    return resp, url


def fetch_tile(name: str, meta: dict[str, tuple[int, str]], retries=(5, 30, 120)):
    """Parse one tile, check size and SHA-256 against meta4; retry on any failure; raise after the last."""
    base = Path(name).name
    size, sha = meta[base]
    last = None
    for attempt in range(len(retries) + 1):
        t0 = time.monotonic()
        try:
            raw, url = open_tile(name, attempt)
            with raw:
                rd = HashingReader(raw)
                recs = list(parse_tile(rd))
                while rd.read(1 << 20):  # trailing bytes after the root still count for the hash
                    pass
            if rd.size != size or rd.h.hexdigest() != sha:
                raise TileError(f"{base}: got {rd.size} B {rd.h.hexdigest()[:12]}, meta4 {size} B {sha[:12]}")
            return recs, {"tile": base, "bytes": rd.size, "sha256": sha, "url": url, "attempt": attempt,
                          "seconds": round(time.monotonic() - t0, 2)}
        except Exception as e:  # noqa: BLE001 — any failure discards the tile's rows
            last = e
            if attempt < len(retries):
                time.sleep(retries[attempt])
    raise TileError(f"{base}: failed after {len(retries) + 1} attempts: {last}")


# --- street / number split ---------------------------------------------------------------------------------
_NUM = r"(\d+)\s*([a-zA-Z]{0,2})"
PATTERNS = [  # order matters: the spaced forms "3 - 5", "12 / 1" would otherwise read as "plain"
    ("range", re.compile(r"^(?P<street>.*\D)\s+(?P<num>\d+)\s*(?P<add>[a-zA-Z]?)\s*[-–]\s*(?P<to>\d+\s*[a-zA-Z]?)$")),
    ("slash", re.compile(r"^(?P<street>.*\D)\s+(?P<num>\d+)\s*/\s*(?P<add>\d+[a-zA-Z]?|[a-zA-Z])$")),
    ("fraction", re.compile(r"^(?P<street>.*\D)\s+(?P<num>\d+)\s+(?P<add>\d/\d)$")),
    ("letter", re.compile(r"^(?P<street>.*\D)\s+(?P<num>\d+)\s*(?P<add>[a-zA-Z]{1,2})$")),
    ("plain", re.compile(r"^(?P<street>.*\D)\s+(?P<num>\d+)$")),
]


_ITEM = r"\d+\s*[a-zA-Z]{0,2}(?:\s*/\s*\d+[a-zA-Z]?)?"
LIST_RX = re.compile(rf"^(?P<street>.*\D)\s+(?P<items>{_ITEM}(?:\s*,\s*{_ITEM})+)$")


def expand(s: str) -> list[dict]:
    """Every house number of a ThoroughfareName: "Bayerstraße 49,51,53" is three numbers of one building."""
    s = re.sub(r"\s+", " ", s or "").strip()
    m = LIST_RX.match(s)
    if not m:
        return [split_thoroughfare(s)]
    street = m.group("street").strip()
    out = []
    for item in re.split(r"\s*,\s*", m.group("items")):
        sp = split_thoroughfare(f"{street} {item}")
        sp["from_list"] = True
        out.append(sp)
    return out


def split_thoroughfare(s: str) -> dict:
    s = re.sub(r"\s+", " ", s or "").strip()
    if not s:
        return {"kind": "empty", "street": "", "num": None, "add": ""}
    if LIST_RX.match(s):
        return {"kind": "list", "street": LIST_RX.match(s).group("street").strip(), "num": None, "add": ""}
    for kind, rx in PATTERNS:
        m = rx.match(s)
        if m:
            street = m.group("street").strip().rstrip(",")
            num, add = int(m.group("num")), (m.groupdict().get("add") or "").lower()
            if kind == "slash" and num >= 11 and num % 10 == 1 and re.fullmatch(r"[2-9]", add):
                # "Gartenau 91/2" is house 9 1/2 written glued: in the whole Land the number before "/2" ends in 1 in
                # 98.7 % of rows (/3 95.5 %, /4 89 %) against 12 % before the letter "a" (measured on the whole Land, 15.11.2026).
                return {"kind": "fraction", "street": street, "num": num // 10, "add": f"1/{add}", "to": None}
            # a street name made only of a number word stays a street ("Straße des 17. Juni 5" works above)
            return {"kind": kind, "street": street, "num": num, "add": add, "to": m.groupdict().get("to")}
    if not re.search(r"\d", s):
        return {"kind": "no_number", "street": s, "num": None, "add": ""}
    return {"kind": "unsplit", "street": s, "num": None, "add": ""}


def summarize(recs, ev):
    from collections import Counter, defaultdict
    fn = Counter(r["function"] for r in recs)
    with_addr = [r for r in recs if r["addresses"]]
    multi = [r for r in recs if len(r["addresses"]) > 1]
    kinds, ex = Counter(), defaultdict(list)
    keys = defaultdict(list)
    loc_by_ags, ags_prefix, extra = defaultdict(Counter), Counter(), Counter()
    for r in recs:
        ags_prefix[(r["ags"] or "")[:2]] += 1
        for a in r["addresses"]:
            sp = split_thoroughfare(a["thoroughfare"])
            kinds[sp["kind"]] += 1
            if len(ex[sp["kind"]]) < 12:
                ex[sp["kind"]].append(f'{a["thoroughfare"]} | {a["locality"]}')
            keys[(r["ags"], a["locality"], a["thoroughfare"])].append(r)
            loc_by_ags[r["ags"]][a["locality"]] += 1
            for t in a["extra_tags"]:
                extra[t] += 1
    spreads = []
    for k, rs in keys.items():
        if len(rs) > 1:
            pts = [(r["x"], r["y"]) for r in rs if r["x"] is not None]
            spreads.append(max(((a[0] - b[0]) ** 2 + (a[1] - b[1]) ** 2) ** 0.5 for a in pts for b in pts))
    spreads.sort()
    res = {
        **ev,
        "buildings": len(recs),
        "by_function": dict(fn.most_common()),
        "residential_31001_1000": fn.get("31001_1000", 0),
        "residential_with_address": sum(1 for r in with_addr if r["function"] == "31001_1000"),
        "with_address": len(with_addr),
        "with_2plus_addresses": len(multi),
        "max_addresses_per_building": max((len(r["addresses"]) for r in recs), default=0),
        "addresses": sum(len(r["addresses"]) for r in recs),
        "distinct_addresses": len(keys),
        "address_on_several_buildings": len(spreads),
        "spread_m_median": round(spreads[len(spreads) // 2], 1) if spreads else None,
        "spread_m_p95": round(spreads[int(len(spreads) * 0.95)], 1) if spreads else None,
        "spread_m_max": round(spreads[-1], 1) if spreads else None,
        "number_kinds": dict(kinds.most_common()),
        "number_examples": dict(ex),
        "ags_prefix": dict(ags_prefix),
        "localities_per_ags": {k: dict(v) for k, v in loc_by_ags.items()},
        "extra_xal_tags": dict(extra),
        "centroid_fallback": sum(1 for r in recs if r["centroid_fallback"]),
        "parts_total": sum(r["parts"] for r in recs),
    }
    return res


def write_rows(recs, path: Path):
    with path.open("w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["gml_id", "alkis_id", "function", "ags", "plan_date", "area", "x", "y", "n_addr", "locality",
                    "thoroughfare"])
        for r in recs:
            for a in (r["addresses"] or [{"locality": "", "thoroughfare": ""}]):
                w.writerow([r["gml_id"], r["alkis_id"], r["function"], r["ags"], r["plan_date"], r["area"],
                            None if r["x"] is None else round(r["x"], 2), None if r["y"] is None else round(r["y"], 2),
                            len(r["addresses"]), a["locality"], a["thoroughfare"]])


def fetch_all(meta4: str, store: Path, pause: float = 0.3, min_free_gib: float = 5.0) -> None:
    """Every tile of meta4 into <store>/tiles/<tile>.csv.gz (addressed buildings only) + one ledger line per tile.

    A tile whose name and SHA-256 are already in the ledger is skipped, so a rerun resumes and a new meta4 fetches
    only changed tiles. Rows go to <tile>.csv.gz.part first and are renamed only after the hash matched. Five failed
    tiles in a row stop the run (the server is refusing); free disk below min_free_gib stops it too.
    """
    import gzip
    import shutil
    meta = load_meta4(meta4)
    tiles_dir = store / "tiles"
    tiles_dir.mkdir(parents=True, exist_ok=True)
    ledger = store / "ledger.jsonl"
    done = set()
    if ledger.exists():
        for line in ledger.read_text().splitlines():
            e = json.loads(line)
            if e.get("status") == "ok":
                done.add((e["tile"], e["sha256"]))
    todo = [n for n in sorted(meta) if (n, meta[n][1]) not in done]
    print(f"tiles {len(meta)}, done {len(meta) - len(todo)}, to fetch {len(todo)}", flush=True)
    fails = 0
    last = 0.0
    t_start = time.monotonic()
    got_bytes = 0
    for i, name in enumerate(todo, 1):
        free = shutil.disk_usage(store).free / 2**30
        if free < min_free_gib:
            print(f"STOP: free {free:.1f} GiB < {min_free_gib}", flush=True)
            return
        wait = pause - (time.monotonic() - last)
        if wait > 0:
            time.sleep(wait)
        gap = time.monotonic() - last if last else None
        last = time.monotonic()
        try:
            recs, ev = fetch_tile(name, meta)
        except TileError as e:
            fails += 1
            with ledger.open("a") as f:
                f.write(json.dumps({"tile": name, "sha256": meta[name][1], "status": "failed", "error": str(e)[:300],
                                    "at": time.strftime("%Y-%m-%dT%H:%M:%S")}) + "\n")
            print(f"FAIL {name}: {e}", flush=True)
            if fails >= 5:
                print("STOP: five failed tiles in a row", flush=True)
                return
            continue
        fails = 0
        kept = [r for r in recs if r["addresses"]]
        part = tiles_dir / f"{Path(name).stem}.csv.gz.part"
        with gzip.open(part, "wt", newline="") as gz:
            w = csv.writer(gz)
            w.writerow(["gml_id", "alkis_id", "function", "ags", "plan_date", "area", "x", "y", "locality", "thoroughfare"])
            for r in kept:
                for a in r["addresses"]:
                    w.writerow([r["gml_id"], r["alkis_id"], r["function"], r["ags"], r["plan_date"], r["area"],
                                None if r["x"] is None else round(r["x"], 3), None if r["y"] is None else round(r["y"], 3),
                                a["locality"], a["thoroughfare"]])
        part.rename(tiles_dir / f"{Path(name).stem}.csv.gz")
        got_bytes += ev["bytes"]
        with ledger.open("a") as f:
            f.write(json.dumps({**ev, "status": "ok", "buildings": len(recs), "with_address": len(kept),
                                "centroid_fallback": sum(1 for r in recs if r["centroid_fallback"]),
                                "gap_s": None if gap is None else round(gap, 3),
                                "at": time.strftime("%Y-%m-%dT%H:%M:%S")}) + "\n")
        if i % 500 == 0 or i == len(todo):
            el = time.monotonic() - t_start
            print(f"{i}/{len(todo)} tiles, {got_bytes / 1e9:.2f} GB, {got_bytes / 1e6 / el:.1f} MB/s, free {free:.1f} GiB",
                  flush=True)


PROVINCE = "BY"  # the DE key is (commune, Land); the Bavarian commune key is its AGS (see canon)


def src_rows(store: Path):
    """One row per house number of every addressed building in the store; the forms nobody can place are counted."""
    import gzip
    from collections import Counter
    dropped = Counter()
    for f in sorted((store / "tiles").glob("*.csv.gz")):
        with gzip.open(f, "rt", newline="") as gz:
            for r in csv.DictReader(gz):
                if not r["x"]:
                    dropped["no_geometry"] += 1
                    continue
                for p in expand(r["thoroughfare"]):
                    if p["kind"] not in ("plain", "letter", "slash", "fraction"):
                        dropped[p["kind"]] += 1
                        continue
                    if p["kind"] == "slash":
                        number_raw = f'{p["num"]}/{p["add"]}'
                    elif p["kind"] == "fraction":
                        number_raw = f'{p["num"]} {p["add"]}'
                    else:
                        number_raw = f'{p["num"]}{p["add"]}'
                    yield (r["gml_id"], f.name[:-7], r["ags"], r["locality"], p["street"], number_raw,
                           float(r["area"] or 0), float(r["x"]), float(r["y"]))
    src_rows.dropped = dict(dropped)


def canon(store: Path, out_parquet: Path, temp_dir: str) -> dict:
    """Store -> Parquet in the column set of the sheet input (the same base_sql as prep/overture.py, §301).

    * code_insee is the 8-digit AGS of the commune ("09162000"): it fits the 8 bytes of the sheet format and cannot
      collide with the "L000001" codes of the Overture rows, so the same Bavarian rows join both DE sheets unchanged.
    * the point is the ground-plan centroid, UTM32 (EPSG:25832) -> WGS84.
    * one address on several buildings (a building cut by a tile edge, or two buildings) keeps the building with the
      largest ground plan; the choice is fixed before any measurement and blind to engine answers.
    """
    import duckdb
    from overture import base_sql
    con = duckdb.connect()
    con.execute("LOAD spatial")
    con.execute(f"SET temp_directory='{temp_dir}'")
    from duck import cap_spill

    cap_spill(con)
    con.execute("CREATE TABLE raw (gml_id VARCHAR, tile VARCHAR, ags VARCHAR, locality VARCHAR, street VARCHAR, "
                "number_raw VARCHAR, area DOUBLE, x DOUBLE, y DOUBLE)")
    batch = []
    for row in src_rows(store):
        batch.append(row)
        if len(batch) >= 200_000:
            con.executemany("INSERT INTO raw VALUES (?,?,?,?,?,?,?,?,?)", batch)
            batch = []
    if batch:
        con.executemany("INSERT INTO raw VALUES (?,?,?,?,?,?,?,?,?)", batch)
    dropped = getattr(src_rows, "dropped", {})
    raw_rows = con.execute("SELECT count(*) FROM raw").fetchone()[0]
    con.execute(f"""CREATE TABLE src AS
        SELECT 'by-lod2:' || gml_id || ':' || number_raw AS id, street AS nom_voie, number_raw,
               '' AS postcode_raw, locality AS nom_commune, '{PROVINCE}' AS provincia, ags, area,
               ST_X(p) AS lon, ST_Y(p) AS lat
        FROM (SELECT *, ST_Transform(ST_Point(x, y), 'EPSG:25832', 'EPSG:4326', always_xy := true) AS p FROM raw)""")
    con.execute(f"""CREATE TABLE b AS
        SELECT base.*, s.ags, s.area FROM ({base_sql('src', 'DE')}) base JOIN src s USING (id)""")
    con.execute("""CREATE TABLE d AS SELECT * FROM (
        SELECT *, row_number() OVER (PARTITION BY ags, nom_voie_norm, numero, coalesce(rep, '')
                                     ORDER BY area DESC, id) AS rn FROM b
        WHERE numero IS NOT NULL AND nom_voie_norm <> '') WHERE rn = 1""")
    con.execute(f"""COPY (
        SELECT id, numero, rep, nom_voie, nom_voie_norm, coalesce(code_postal, '0') AS code_postal,
               postcode_raw AS code_postal_display, ags AS code_insee, nom_commune, nom_commune_norm,
               '' AS provincia_norm, nom_voie_norm || '|' || ags AS street_key, lon, lat
        FROM d ORDER BY nom_voie_norm, ags, numero, rep) TO '{out_parquet}' (FORMAT parquet)""")
    stats = {
        "raw_numbers": raw_rows,
        "dropped_forms": dropped,
        "after_base": con.execute("SELECT count(*) FROM b").fetchone()[0],
        "no_number_or_street": con.execute("SELECT count(*) FROM b WHERE numero IS NULL OR nom_voie_norm = ''").fetchone()[0],
        "duplicates_removed": con.execute("SELECT count(*) FROM b WHERE numero IS NOT NULL AND nom_voie_norm <> ''").fetchone()[0]
        - con.execute("SELECT count(*) FROM d").fetchone()[0],
        "rows_kept": con.execute("SELECT count(*) FROM d").fetchone()[0],
        "communes": con.execute("SELECT count(DISTINCT ags) FROM d").fetchone()[0],
        "non_bavarian_ags": con.execute("SELECT count(*) FROM d WHERE ags NOT LIKE '09%'").fetchone()[0],
        "names_per_ags_max": con.execute("SELECT max(n) FROM (SELECT count(DISTINCT nom_commune) n FROM d GROUP BY ags)").fetchone()[0],
        "bbox": con.execute("SELECT min(lon), min(lat), max(lon), max(lat) FROM d").fetchone(),
    }
    return stats


BUILD_COLS = ("nom_voie_norm", "code_insee", "nom_commune_norm", "code_postal", "code_postal_display", "numero", "rep",
              "lon", "lat", "nom_voie", "nom_commune", "provincia_norm")
# the expressions of prep/export_build.py, applied to the Bavarian canon only; the base CSV is already in that form
_ORD = "regexp_replace(nom_voie_norm, '(^| )0+([1-9][0-9]*(st|nd|rd|th))( |$)', '\\1\\2\\4', 'g')"
# an empty string is written as NULL: DuckDB prints '' as "" and NULL as nothing, and the base stream has nothing
_BY_SELECT = (f"trim(regexp_replace({_ORD}, '\\s+', ' ', 'g')) AS nom_voie_norm, code_insee, "
              "trim(regexp_replace(nom_commune_norm, '\\s+', ' ', 'g')) AS nom_commune_norm, "
              "nullif(code_postal, '') AS code_postal, nullif(code_postal_display, '') AS code_postal_display, "
              "CAST(numero AS VARCHAR) AS numero, "
              "nullif(lower(regexp_replace(rep, '[\\s\\-/\\.ʻʼ‘]', '', 'g')), '') AS rep, "
              "CAST(lon AS VARCHAR) AS lon, CAST(lat AS VARCHAR) AS lat, nom_voie, nom_commune, "
              "nullif(provincia_norm, '') AS provincia_norm")


def build_input(base_csv: str, by_parquet: str | None, out_csv: str, temp_dir: str) -> dict:
    """Builder CSV of an existing sheet + the Bavarian canon -> one builder CSV in the order of prep/export_build.py.

    The base rows are carried as text (all_varchar; an empty field reads as NULL, so the sort coalesces it as the
    builder input does: "15" before "15a"): no value of the 15 Länder is re-parsed or re-printed, so a run
    without Bavaria reproduces the base stream and the sheet built from it (the neutrality control).
    """
    import duckdb
    con = duckdb.connect()
    con.execute(f"SET temp_directory='{temp_dir}'")
    cols = ", ".join(BUILD_COLS)
    union = f"SELECT {cols} FROM read_csv('{base_csv}', all_varchar=true, header=true)"
    if by_parquet:
        union += f" UNION ALL SELECT {_BY_SELECT} FROM read_parquet('{by_parquet}')"
    con.execute(f"""COPY (SELECT {cols} FROM ({union})
        ORDER BY nom_voie_norm, code_insee, CAST(numero AS INTEGER), coalesce(rep, ''), CAST(lon AS DOUBLE),
                 CAST(lat AS DOUBLE))
        TO '{out_csv}' (FORMAT csv, HEADER true, COMPRESSION gzip)""")
    rows = con.execute(f"SELECT count(*), count(*) FILTER (WHERE code_insee LIKE '09%') FROM read_csv('{out_csv}', "
                       "all_varchar=true, header=true)").fetchone()
    return {"rows": rows[0], "bavarian_rows": rows[1]}



# --- the sheet passport with Bavaria (ANSWERS §339) -------------------------------------------------------------
# LoD2 joins the German sheet as a supplement next to the catalog of the 15 Länder, as the BNetzA postcode witness
# does in the paid sheet: prep/de_sources.py keeps that catalog exact (DE-BY is forbidden there), so the Bavarian
# record lives under its own key and the catalog keeps its own hash.

CARD = Path(__file__).resolve().parent / "de_lod2_bavaria_source_v1.json"
ATTRIBUTION = "Bayerische Vermessungsverwaltung – www.geodaten.bayern.de"  # the form the terms require
LICENSE = "CC BY 4.0"
LICENSE_URL = "https://creativecommons.org/licenses/by/4.0/deed.de"
LICENSE_NAME = "Creative Commons Attribution 4.0 International"
COVERAGE = "16/16 Länder; Bavaria from the LDBV building models (LoD2), without postcodes"


def card_problems(card: dict) -> list[str]:
    """What a Bavarian source card must say before a sheet may carry its rows."""
    problems = []
    if card.get("schema") != "gridpin-de-lod2-bavaria-source-card-v1":
        problems.append("schema is not the LoD2 source card")
    if card.get("land") != "DE-BY":
        problems.append("land is not DE-BY")
    if card.get("license") != LICENSE or card.get("license_url") != LICENSE_URL:
        problems.append("licence is not CC BY 4.0 with its link")
    if card.get("attribution") != ATTRIBUTION:
        problems.append("attribution differs from the form the terms require")
    if ATTRIBUTION not in str((card.get("terms_snapshot") or {}).get("required_attribution_form", "")):
        problems.append("the terms snapshot does not carry the required attribution form")
    for key in ("catalog_snapshot",):
        if not re.fullmatch(r"[0-9a-f]{64}", str(((card.get("product") or {}).get(key) or {}).get("sha256", ""))):
            problems.append(f"product.{key} has no SHA-256")
    for section, key in (("terms_snapshot", "sha256"), ("release", "metalink_sha256"), ("canon", "sha256")):
        if not re.fullmatch(r"[0-9a-f]{64}", str((card.get(section) or {}).get(key, ""))):
            problems.append(f"{section}.{key} is not a SHA-256")
    if not card.get("transformations"):
        problems.append("no indication of changes")
    if any(t in json.dumps(card).lower() for t in ("odbl", "share-alike", "share alike")):
        problems.append("a copyleft term in a permissive source card")
    return problems


def _supplement(card: dict) -> dict:
    rel, canon = card["release"], card["canon"]
    changes = "; ".join(card["transformations"])
    return {
        "land": "Bayern",
        "source": card["product"]["title"],
        "provider": card["provider"],
        "license": LICENSE_NAME,
        "license_url": LICENSE_URL,
        "source_url": card["release"]["metalink_url"],
        "published": rel["published"],
        "rows": canon["rows"],
        "canon_sha256": canon["sha256"],
        "transformations": list(card["transformations"]),
        "attribution": f"{ATTRIBUTION}; {LICENSE_NAME} ({LICENSE_URL}); source: {card['product']['title']}, "
                       f"published {rel['published'][:10]}; changes: {changes}",
    }


def with_bavaria(manifest: dict, card: dict) -> dict:
    """The passport of a German sheet whose input carries the Bavarian canon: the base passport plus LoD2."""
    problems = card_problems(card)
    if problems:
        raise ValueError("LoD2 source card: " + "; ".join(problems))
    coverage = str(manifest.get("coverage", ""))
    if "supplement_catalog" in manifest or not re.search(r"Bavaria (remains )?not covered", coverage):
        raise ValueError(f"the base passport must say Bavaria is not covered, got {coverage!r}")
    out = dict(manifest)
    entry = _supplement(card)
    for key, suffix in _suffixes(entry).items():
        out[key] = f"{manifest[key]}{suffix}"
    out["base_coverage"] = manifest["coverage"]
    out["coverage"] = COVERAGE
    out["supplement_catalog"] = {"DE-BY": entry}
    out["supplement_catalog_sha256"] = _sha({"DE-BY": entry})
    return out


BAVARIA_KEYS = ("supplement_catalog", "supplement_catalog_sha256", "base_coverage")


def _sha(value) -> str:
    return hashlib.sha256(
        json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def _suffixes(entry: dict) -> dict:
    """What LoD2 appends to the base passport, field by field."""
    return {
        "license": f"; Bavaria: {LICENSE_NAME} (Bayerische Vermessungsverwaltung)",
        "sources": f"; Bavaria: Bayerische Vermessungsverwaltung, {entry['source']} {entry['published'][:10]}",
        "attribution": f"; Bavaria: {entry['attribution']}",
    }


def load_card() -> dict:
    return json.loads(CARD.read_text(encoding="utf-8"))


def _decoded(value):
    if isinstance(value, str):
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return value
    return value


def without_bavaria(manifest: dict, card: dict) -> dict:
    """The base passport a Bavarian passport was built from (ANSWERS §355).

    The release gate checks the base exactly as before, so a passport passes only when it is the 15-Länder passport
    plus LoD2 as the card describes it; anything else (a changed suffix, a second record, a coverage line written by
    hand) raises ValueError.
    """
    problems = bavaria_manifest_problems(manifest, card)
    if problems:
        raise ValueError("; ".join(problems))
    entry = _supplement(card)
    base = {k: v for k, v in manifest.items() if k not in BAVARIA_KEYS}
    for key, suffix in _suffixes(entry).items():
        value = str(manifest.get(key, ""))
        if not value.endswith(suffix):
            raise ValueError(f"{key} is not the base passport plus the LoD2 record")
        base[key] = value[: -len(suffix)]
    if not manifest.get("base_coverage"):
        raise ValueError("no base_coverage: the 15-Länder coverage line is lost")
    base["coverage"] = manifest["base_coverage"]
    rebuilt = with_bavaria(base, card)
    for key in ("license", "sources", "attribution", "coverage", "base_coverage", "supplement_catalog_sha256"):
        if str(rebuilt[key]) != str(manifest.get(key)):
            raise ValueError(f"{key} differs from the base passport plus the LoD2 record")
    if _decoded(manifest.get("supplement_catalog")) != rebuilt["supplement_catalog"]:
        raise ValueError("supplement_catalog differs from the LoD2 record of the card")
    return base


def bavaria_manifest_problems(manifest: dict, card: dict) -> list[str]:
    """A passport of a sheet with Bavarian rows: everything the licence asks for is there and matches the card."""
    sup = manifest.get("supplement_catalog")
    if isinstance(sup, str):
        try:
            sup = json.loads(sup)
        except json.JSONDecodeError:
            return ["supplement_catalog is not JSON"]
    entry = (sup or {}).get("DE-BY") if isinstance(sup, dict) else None
    if not entry:
        return ["no DE-BY record: the Bavarian rows have no attribution"]
    problems = []
    want = _supplement(card)
    for key in ("license", "license_url", "attribution", "canon_sha256", "rows"):
        if entry.get(key) != want[key]:
            problems.append(f"DE-BY {key} differs from the source card")
    if ATTRIBUTION not in str(manifest.get("attribution", "")):
        problems.append("the passport attribution does not name the Bayerische Vermessungsverwaltung")
    if LICENSE_NAME not in str(manifest.get("license", "")):
        problems.append("the passport licence does not name CC BY 4.0")
    if manifest.get("coverage") != COVERAGE:
        problems.append("coverage does not say how Bavaria is covered")
    return problems


def main(argv):
    if argv[0] == "manifest":  # manifest <base_manifest.json> <out.json>: the passport with Bavaria (card CARD)
        card = json.loads(CARD.read_text(encoding="utf-8"))
        out = with_bavaria(json.loads(Path(argv[1]).read_text(encoding="utf-8")), card)
        Path(argv[2]).write_text(json.dumps(out, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(json.dumps({"coverage": out["coverage"], "supplement_catalog_sha256": out["supplement_catalog_sha256"]}))
        return
    if argv[0] == "build-input":  # build-input <base.csv.gz> <bavaria.parquet|-> <out.csv.gz> <temp_dir>
        print(json.dumps(build_input(argv[1], None if argv[2] == "-" else argv[2], argv[3], argv[4])))
        return
    if argv[0] == "canon":
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        stats = canon(Path(argv[1]), Path(argv[2]), argv[3])
        Path(str(argv[2]) + ".stats.json").write_text(json.dumps(stats, ensure_ascii=False, indent=1))
        print(json.dumps(stats, ensure_ascii=False))
        return
    if argv[0] == "fetch":
        pause = float(argv[argv.index("--pause") + 1]) if "--pause" in argv else 0.3
        fetch_all(argv[1], Path(argv[2]), pause=pause)
        return
    if argv[0] == "tile":
        name, meta4, out = argv[1], argv[2], Path(argv[3])
        out.mkdir(parents=True, exist_ok=True)
        meta = load_meta4(meta4)
        recs, ev = fetch_tile(name, meta)
        base = Path(name).stem
        s = summarize(recs, ev)
        (out / f"{base}.json").write_text(json.dumps(s, ensure_ascii=False, indent=1))
        write_rows(recs, out / f"{base}.csv")
        print(json.dumps({k: s[k] for k in ("tile", "bytes", "seconds", "buildings", "with_address", "addresses",
                                            "distinct_addresses", "number_kinds")}, ensure_ascii=False))


if __name__ == "__main__":
    main(sys.argv[1:])
