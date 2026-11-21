#!/usr/bin/env python3
"""POI layer from Overture places (separate index — do not mix with the address index).

Mixing POIs into the address pool degrades address matching, so this layer is built
into a SEPARATE {cc}_poi_norm.parquet -> {cc}_poi.bin; isolation is enforced by a
cascade on the caller side (address index first, POI only on empty/city-level/weak
results). License: CDLA-Permissive-2.0, Overture attribution in NOTICE.

Schema is compatible with the index builder: POI name -> nom_voie, Overture city ->
commune, synthetic house number 1; the commune key includes a 0.5° geo cell so that
same-named cities stay apart. Quality filter: confidence threshold plus name length
4..80 chars.

Usage: python3 prep/overture_places_layer.py FR [--release 2026-10-21.0]   (from the code/ directory)
`--release` picks the Overture release (default: OVERTURE_RELEASE of prep/duck.py); the arrondissement
communes of Paris, Lyon and Marseille are derived only for FR (other countries share those postcode digits).
"""
import json
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from duck import connect_tuned
from osm import NORM
from overture import assert_no_copyleft

CODE = pathlib.Path(__file__).resolve().parent.parent
from duck import OVERTURE_RELEASE as RELEASE  # single source
S3 = f"s3://overturemaps-us-west-2/release/{RELEASE}/theme=places/type=*/*.parquet"

# Overture PLACES is a MIX of permissive licenses, not one CDLA (, verified against
# docs.overturemaps.org/guides/places/ + /attribution/, 2026-07): CDLA-Permissive-2.0 (Meta,
# Microsoft, PinMeTo, …), Apache-2.0 (Foursquare), CC0-1.0 (AllThePlaces).
PLACES_LICENSE = "mixed permissive (CDLA-Permissive-2.0 / Apache-2.0 / CC0-1.0)"
PLACES_ATTRIBUTION = ("© Overture Maps Foundation and contributors; per-source: "
                      "CDLA-Permissive-2.0 (Meta, Microsoft, …), Apache-2.0 (Foursquare), "
                      "CC0-1.0 (AllThePlaces)")


def raw_cache_path(cc: str, release: str = RELEASE) -> pathlib.Path:
    """Content-addressed raw-extract cache by (release, theme=places, cc): a pin bump changes the
    path, so old bytes can NEVER be re-tagged with a new source_release. The legacy
    release-agnostic `data/<cc>_poi_raw.parquet` is orphaned by this and may be deleted."""
    return CODE / "data" / f"{cc.lower()}_poi_raw__{release}.parquet"


def truncate_display_255(s: str, max_bytes: int = 255) -> str:
    """Cut a DISPLAY name to <=255 UTF-8 BYTES at a character boundary. The builder's names
    section stores a u8 byte-length, so a >255-byte name (styled unicode like Mathematical Bold,
    4 bytes/char, passes the 80-CHAR filter at up to ~320 bytes) fails the whole monthly build.
    Display-only (not identity: nom_voie_norm is derived from the truncated form too), so a cut
    tail on a decorative name is acceptable; the builder's hard reject stays as the safety net."""
    b = s.encode("utf-8")
    if len(b) <= max_bytes:
        return s
    # decode-with-ignore drops the trailing partial character cleanly at the byte cap
    return b[:max_bytes].decode("utf-8", errors="ignore").rstrip()


def places_manifest(cc: str, release: str = RELEASE) -> dict:
    """Provenance manifest for a POI sheet. Pure function of the single source of truth so the
    manifest regenerator can call it WITHOUT the S3 pull."""
    return {
        "country": cc.lower(),
        "layer": "poi",
        "license": PLACES_LICENSE,
        "sources": "Overture Maps places (Meta, Microsoft, Foursquare, AllThePlaces, …)",
        "source_release": release,
        "attribution": PLACES_ATTRIBUTION,
    }


# Serbian Cyrillic -> Gaj's Latin, the 1:1 pair of norm::translit_cyr_lat. Overture writes the places of Serbia in
# Latin and their localities in Cyrillic (Nis, Beograd written in Cyrillic), while people write either script for either part; the
# layer keyed in one script missed both (ANSWERS §337, 15.11.2026: name + city 88 -> 214 of 1 000). The engine reads
# a Cyrillic query in Latin too, so one script in the layer serves both.
_GAJ = dict(zip("абвгдђежзијклмнопрстћуфхцчш", "abvgdđežzijklmnoprstćufhcčš")) | {"љ": "lj", "њ": "nj", "џ": "dž",
        # the Russian letters outside the Serbian set, as the engine maps them (Russian shop names in every layer)
        "я": "ja", "ю": "ju", "щ": "šč", "ы": "i", "э": "e", "ь": "", "ъ": "",
        "ё": "e", "й": "i"}  # the engine reads yo and short i after normalize() folds them to e and i
CYRILLIC_RE = "[\u0400-\u04FF]"


def gaj_latin(s: str | None) -> str | None:
    """Cyrillic letters of the Serbian alphabet to Latin, case kept; every other character as it is."""
    if s is None:
        return None
    out = []
    for ch in s:
        lo = ch.lower()
        t = _GAJ.get(lo)
        out.append(ch if t is None else (t.capitalize() if ch != lo else t))
    return "".join(out)


def normalize(con, raw, cc: str) -> None:
    """Raw Overture places of one country -> table `canon` in the address-builder schema (POI name as the
    street, Overture locality as the commune, synthetic house number 1). Pure of I/O besides reading `raw`,
    so the tests run it on a tiny extract."""
    norm_name = NORM.format(col="gaj(nom_voie)")
    con.create_function("gaj_udf", gaj_latin, ["VARCHAR"], "VARCHAR")
    con.execute(f"CREATE OR REPLACE MACRO gaj(x) AS CASE WHEN regexp_matches(x, '{CYRILLIC_RE}') THEN gaj_udf(x) ELSE x END")
    # Display names are capped at 255 UTF-8 BYTES (the builder's u8 length prefix): the 80-CHAR
    # filter passes styled unicode (4 bytes/char) at up to ~320 bytes, and ONE such name fails the
    # whole monthly build. Truncate at a char boundary (UDF below, CASE-gated so it runs only on
    # offenders); the builder's hard reject stays as the safety net.
    def _trunc255_udf(s: str) -> str:
        return truncate_display_255(s)
    con.create_function("trunc255", _trunc255_udf)
    over = con.execute(f"""
        SELECT count(*) FROM (
            SELECT unnest(list_distinct(list_filter(
                       list_append(list_append(name_common, name_primary), brand_name),
                       x -> x IS NOT NULL AND len(x) BETWEEN 4 AND 80))) AS nom_voie
            FROM read_parquet('{raw}')
        ) WHERE strlen(nom_voie) > 255
    """).fetchone()[0]
    if over:
        print(f"  display names over 255 bytes: truncated {over} (u8 length prefix of the names section)")
    # EXPLODE by name variants (primary + common + brand — same as a_streets in osm.py):
    # official names often live in `common`, trade names in `primary`.
    # Arrondissement cities: locality "Paris" is too flat — derive the arrondissement
    # commune from postcodes 75NNN/69NNN/13NNN (compatible with the address index and
    # fr_arrondissement_rewrite).
    con.execute(f"""
        CREATE TABLE poi AS
        WITH raw AS (SELECT * FROM read_parquet('{raw}')),
        ex AS (
            SELECT CASE WHEN strlen(nv) > 255 THEN trunc255(nv) ELSE nv END AS nom_voie,
                   locality, code_postal_raw, categorie, confidence, lon, lat
            FROM (
                SELECT unnest(list_distinct(list_filter(
                           list_append(list_append(name_common, name_primary), brand_name),
                           x -> x IS NOT NULL AND len(x) BETWEEN 4 AND 80))) AS nv,
                       locality, code_postal_raw, categorie, confidence, lon, lat
                FROM raw
            )
        )
        , arr AS (
        SELECT nom_voie,
               CASE
                 WHEN '{cc}' <> 'FR' THEN nom_commune_src
                 WHEN pc5 BETWEEN '75001' AND '75020'
                   THEN 'Paris ' || CAST(CAST(pc5 AS INT) % 100 AS VARCHAR)
                        || CASE WHEN pc5 = '75001' THEN 'er' ELSE 'e' END || ' Arrondissement'
                 WHEN pc5 BETWEEN '69001' AND '69009'
                   THEN 'Lyon ' || CAST(CAST(pc5 AS INT) % 100 AS VARCHAR)
                        || CASE WHEN pc5 = '69001' THEN 'er' ELSE 'e' END || ' Arrondissement'
                 WHEN pc5 BETWEEN '13001' AND '13016'
                   THEN 'Marseille ' || CAST(CAST(pc5 AS INT) % 100 AS VARCHAR)
                        || CASE WHEN pc5 = '13001' THEN 'er' ELSE 'e' END || ' Arrondissement'
                 ELSE nom_commune_src
               END AS nom_commune,
               code_postal_raw, categorie, confidence, lon, lat
        FROM (SELECT ex.*, ex.locality AS nom_commune_src,
                     -- Overture postcode field is dirty ("75007 Paris ") — take the leading 5 digits
                     -- doubled braces: this SQL is an f-string, and single braces around the 5 made it the literal 5, so the
                     -- arrondissement communes were never derived (14.11.2026, test_places_layer_…_only_in_france)
                     coalesce(regexp_extract(ex.code_postal_raw, '^[0-9]{{5}}'), '') AS pc5
              FROM ex)
        )
        -- a place of an arrondissement is also a place of its city: people write «Musée du Louvre, Paris», and a
        -- commune «Paris» of the places without a postcode answered that query from one of them, 4 km off, while the
        -- museum itself sat in «Paris 1er Arrondissement» (15.11.2026, ANSWERS §337). The copy under the city name
        -- joins the grouping below, so the city keeps the place Overture is surest of.
        SELECT * FROM arr
        UNION ALL
        SELECT * REPLACE (split_part(nom_commune, ' ', 1) AS nom_commune)
        FROM arr WHERE nom_commune LIKE '% Arrondissement'
    """)
    con.execute(f"""
        CREATE TABLE loc AS
        WITH base AS (
            SELECT nom_voie, {norm_name} AS nom_voie_norm,
                   gaj(nom_commune) AS nom_commune, {NORM.format(col="gaj(nom_commune)")} AS nom_commune_norm,
                   -- a postcode that starts with a letter (CA "V9Z 1K2") has no numeric key: its first digits
                   -- would be a false postcode (PRINCIPLES 2.8-twelve, fourth source)
                   CASE WHEN regexp_matches(trim(code_postal_raw), '^[0-9]')
                        THEN regexp_extract(code_postal_raw, '[0-9]+') ELSE '' END AS code_postal,
                   code_postal_raw AS code_postal_display,
                   confidence, lon, lat
            FROM poi
            WHERE lon IS NOT NULL AND lat IS NOT NULL
        ),
        -- the country's postcode length is the most common one; scraps of other lengths ("3", "34") are dropped:
        -- the builder rejects a street whose postcode dictionary is implausible (ES, 14.11.2026)
        pc_len AS (SELECT mode(length(code_postal)) AS n FROM base WHERE code_postal <> ''),
        l AS (
            SELECT * REPLACE (CASE WHEN length(code_postal) = (SELECT n FROM pc_len) THEN code_postal ELSE '' END
                              AS code_postal),
                   'P' || lpad(CAST(dense_rank() OVER (
                       ORDER BY nom_commune_norm, floor(lat*2), floor(lon*2)) AS VARCHAR), 6, '0')
                   AS code_insee
            FROM base
            WHERE nom_voie_norm <> ''
        )
        SELECT * FROM l
    """)
    # the places table is not needed past this point: dropping it before the grouping keeps the US build within the
    # memory and the temporary space of the machine (14.11.2026: one statement spilled past 7.8 GiB and stopped)
    con.execute("DROP TABLE poi")
    con.execute("""
        CREATE TABLE canon AS
        WITH
        -- one place per name and commune: same-named places of one town (a station, a gym and a car park all
        -- «Union Station», Chicago) were one street to the builder, and the answer was the middle of them, where
        -- none of them is; the place Overture is surest of stays, at a tie the southernmost, then westernmost
        -- (14.11.2026, ANSWERS §336, kind 7 of the check). A grouping, not a window: a window over every place of
        -- the US ran out of memory.
        pick AS (
            SELECT nom_voie_norm, code_insee,
                   arg_max({'nom_voie': nom_voie, 'code_postal': code_postal,
                             'code_postal_display': code_postal_display, 'nom_commune': nom_commune,
                             'nom_commune_norm': nom_commune_norm, 'lon': lon, 'lat': lat},
                           {'c': coalesce(confidence, -1), 's': -lat, 'w': -lon}) AS p
            FROM loc
            GROUP BY nom_voie_norm, code_insee
        )
        SELECT 1 AS numero, '' AS rep, p.nom_voie, nom_voie_norm,
               coalesce(p.code_postal, '0') AS code_postal, p.code_postal_display,
               code_insee, p.nom_commune, p.nom_commune_norm,
               '' AS provincia_norm,
               nom_voie_norm || '|' || code_insee AS street_key,
               p.lon, p.lat
        FROM pick
    """)
    con.execute("DROP TABLE loc")


def main():
    args = sys.argv[1:]
    release = RELEASE
    if len(args) == 3 and args[1] == "--release":
        release = args[2]
    elif len(args) != 1:
        sys.exit("usage: prep/overture_places_layer.py <COUNTRY_CODE> [--release <overture release>]")
    cc = args[0].upper()
    s3 = f"s3://overturemaps-us-west-2/release/{release}/theme=places/type=*/*.parquet"
    out = CODE / "data" / f"{cc.lower()}_poi_norm.parquet"
    t0 = time.time()
    con = connect_tuned()
    con.execute("INSTALL spatial; LOAD spatial;")
    con.execute("INSTALL httpfs; LOAD httpfs;")
    con.execute("SET s3_region='us-west-2';")
    con.execute("SET http_timeout=120000;")
    con.execute("SET http_retries=8;")
    raw = raw_cache_path(cc, release)
    sidecar = raw.with_suffix(".meta.json")
    if not raw.exists():
        # cache the raw extract on disk: the layer can be re-sliced without a fresh S3 scan (~30 min)
        con.execute(f"""
            COPY (
                SELECT names.primary AS name_primary,
                       coalesce(map_values(names.common), []) AS name_common,
                       brand.names.primary AS brand_name,
                       coalesce(addresses[1].locality, '') AS locality,
                       coalesce(addresses[1].postcode, '') AS code_postal_raw,
                       basic_category AS categorie,
                       confidence,
                       st_x(geometry) AS lon, st_y(geometry) AS lat
                FROM read_parquet('{s3}', hive_partitioning=1)
                WHERE addresses[1].country = '{cc}'
                  AND confidence >= 0.30
                  AND names.primary IS NOT NULL
                  AND len(names.primary) BETWEEN 4 AND 80
            ) TO '{raw}' (FORMAT parquet, COMPRESSION zstd)
        """)
        # provenance sidecar: pin the raw extract to its release + row digest
        rows = con.execute(f"SELECT count(*) FROM read_parquet('{raw}')").fetchone()[0]
        sidecar.write_text(json.dumps(
            {"source_release": release, "s3": s3, "rows": rows}, ensure_ascii=False, indent=2))
    else:
        # reuse branch: the path already encodes the release, but verify the sidecar agrees so a
        # hand-copied/renamed cache can't get a new source_release stamped onto old bytes
        try:
            meta = json.loads(sidecar.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            sys.exit(f"STOP: cache {raw.name} has no provenance sidecar — delete and re-pull "
                     f"(`rm -f {raw}`; re-run make fr-poi)")
        if meta.get("source_release") != release:
            sys.exit(f"STOP: cache {raw.name} is from release {meta.get('source_release')}, current is "
                     f"{release} — delete and re-pull (`rm -f {raw} {sidecar}`)")
    normalize(con, raw, cc)
    kept = con.execute("SELECT count(*) FROM canon").fetchone()[0]
    con.execute(f"COPY canon TO '{out}' (FORMAT parquet, COMPRESSION zstd)")
    sources = [{"name": "Overture Maps — places layer (POI)", "release": release,
                "license": PLACES_LICENSE, "attribution": PLACES_ATTRIBUTION}]
    assert_no_copyleft(cc, sources)  # license gate: keep this layer free of copyleft sources
    stats = {"cc": cc, "layer": "poi", "kept": kept, "out": str(out),
             "sources": sources, "seconds": round(time.time() - t0, 1)}
    (CODE / "data" / f"{cc.lower()}_poi_stats.json").write_text(
        json.dumps(stats, ensure_ascii=False, indent=2))
    # provenance manifest -> SEC_META of the POI sheet (v6)
    (CODE / "data" / f"{cc.lower()}poi_manifest.json").write_text(
        json.dumps(places_manifest(cc, release), ensure_ascii=False, indent=2))
    print(json.dumps(stats, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
