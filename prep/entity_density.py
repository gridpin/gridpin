"""How many businesses are registered at each house of France: the SIRENE register of establishments counted per
house of the sheet, a layer for `gridpin-pro batch --append entities`.

    python3 prep/entity_density.py OUT.tsv.gz [--sirene URL_OR_PATH] [--ban data/ban_norm.parquet] [--reference 2026-11-03]

SIRENE (INSEE, Licence Ouverte 2.0) gives each establishment the identifier of its street in BAN
(`identifiantAdresseEtablissement`) and its number; BAN is the source of the sheet of France (`data/ban_norm.parquet`),
so the house is found by street identifier and number, not by parsing the address (see JOINED). The key of a house is what the sheet answers for it:
street, house number with its repetition ("188b"), postcode, lower case, joined by '|'.

Per house: `active` establishments (état A), `ever` (all states), `median_age_years` of the active ones (from their
creation date to the reference date, the date of the SIRENE file), `registered_12m` (created in the 12 months before
the reference date). Establishments whose address the register does not publish (statut de diffusion P: a person who
objects) are not counted at all; they are reported by number. The layer is a count from the register, not a risk
score: nothing in it says what a high count means.

The file: a header line `#gridpin entity layer 1` with key=value fields, then tab-separated rows sorted by key:
key, active, ever, median_age_years, registered_12m.
"""
import argparse
import gzip
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from duck import connect_tuned  # noqa: E402

SIRENE = ("https://static.data.gouv.fr/resources/base-sirene-des-entreprises-et-de-leurs-etablissements-siren-siret/"
          "20261103-101714/stock-stocketablissement-parquet.parquet")

# SIRENE's identifiantAdresseEtablissement names the street, not the house: "132045859_B" is commune 13204, street
# 5859 (checked 17.11.2026: houses 61 and 51 of one street share it). A BAN house id is commune_street_number[_rep]
# ("13204_5859_00045", "06083_0860_00087_bis"), and BAN's street part can be longer than SIRENE's four characters
# ("84007_cvsrg4_…" against "84007cvsr_B"). The join key is commune, the first four characters of the street, the
# number and the repetition; a key that two BAN houses share is ambiguous and not counted.
# Plain strings, not f-strings (PRINCIPLES 2.7): the parameters are bound by DuckDB.
JOINED = """
WITH s AS (
    SELECT identifiantAdresseEtablissement AS sid,
           etatAdministratifEtablissement AS state,
           statutDiffusionEtablissement AS diffusion,
           dateCreationEtablissement AS created,
           codePaysEtrangerEtablissement AS abroad,
           left(identifiantAdresseEtablissement, 5) || '|' || substr(identifiantAdresseEtablissement, 6, 4) || '|'
               || try_cast(numeroVoieEtablissement AS INTEGER) || '|'
               || CASE upper(coalesce(indiceRepetitionEtablissement, ''))
                      WHEN '' THEN '' WHEN 'B' THEN 'bis' WHEN 'T' THEN 'ter' WHEN 'Q' THEN 'quater'
                      WHEN 'C' THEN 'quinquies' ELSE lower(indiceRepetitionEtablissement) END AS jkey
    FROM read_parquet(?)
), ban AS (
    SELECT split_part(id, '_', 1) || '|' || left(split_part(id, '_', 2), 4) || '|' || numero || '|' || coalesce(rep, '')
               AS jkey,
           lower(nom_voie) || '|' || lower(CAST(numero AS VARCHAR) || coalesce(rep, '')) || '|' || code_postal AS key
    FROM read_parquet(?)
    WHERE numero IS NOT NULL AND numero > 0
), bank AS (
    SELECT jkey, any_value(key) AS key, count(*) AS houses FROM ban GROUP BY jkey
)
SELECT s.*, bank.key, bank.houses,
    CASE
        WHEN diffusion = 'P' THEN 'not_published'
        WHEN abroad IS NOT NULL THEN 'abroad'
        WHEN sid IS NULL OR sid = '' OR jkey IS NULL THEN 'no_address_id_or_number'
        WHEN bank.key IS NULL THEN 'address_not_a_house_of_the_sheet'
        WHEN bank.houses > 1 THEN 'ambiguous_house'
        ELSE 'counted'
    END AS fate
FROM s LEFT JOIN bank USING (jkey)
"""

COUNTS_SQL = "SELECT fate, state, count(*) AS n FROM joined GROUP BY ALL ORDER BY fate, state"

LAYER_SQL = """
SELECT key,
       count(*) FILTER (WHERE state = 'A') AS active,
       count(*) AS ever,
       median(date_diff('day', created, CAST(? AS DATE)) / 365.25) FILTER (WHERE state = 'A' AND created IS NOT NULL)
           AS median_age_years,
       count(*) FILTER (WHERE created > CAST(? AS DATE) - INTERVAL 12 MONTH AND created <= CAST(? AS DATE))
           AS registered_12m
FROM joined WHERE fate = 'counted'
GROUP BY key
ORDER BY key
"""


def build(out: pathlib.Path, sirene: str, ban: str, reference: str) -> dict:
    con = connect_tuned()
    if sirene.startswith("http"):
        con.execute("INSTALL httpfs; LOAD httpfs;")
    con.execute("CREATE TEMP TABLE joined AS " + JOINED, [sirene, ban])
    fates = con.execute(COUNTS_SQL).fetchall()
    rows = con.execute(LAYER_SQL, [reference, reference, reference]).fetchall()
    tmp = out.with_name(out.name + ".part")
    with gzip.open(tmp, "wt", encoding="utf-8", compresslevel=9) as f:
        f.write(f"#gridpin entity layer 1\tcountry=FR\treference={reference}\tsource=SIRENE StockEtablissement "
                f"(INSEE, Licence Ouverte 2.0)\tcolumns=key,active,ever,median_age_years,registered_12m\n")
        for key, active, ever, age, new in rows:
            age_s = "" if age is None else f"{age:.1f}"
            f.write(f"{key.replace(chr(9), ' ')}\t{active}\t{ever}\t{age_s}\t{new}\n")
    tmp.replace(out)
    summary = {"houses": len(rows), "fates": [{"fate": a, "state": b, "establishments": n} for a, b, n in fates]}
    out.with_suffix(".summary.json").write_text(json.dumps(summary, indent=1) + "\n")
    return summary


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out", type=pathlib.Path)
    ap.add_argument("--sirene", default=SIRENE)
    ap.add_argument("--ban", default=str(HERE.parent / "data" / "ban_norm.parquet"))
    ap.add_argument("--reference", default="2026-11-03", help="the date of the SIRENE file (ages and the 12 months)")
    args = ap.parse_args(argv)
    s = build(args.out, args.sirene, args.ban, args.reference)
    print(f"{args.out}: {s['houses']} houses", file=sys.stderr)
    for f in s["fates"]:
        print(f"  {f['fate']:<34} {f['state'] or '-'} {f['establishments']:>12}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
