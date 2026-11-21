#!/usr/bin/env python3
"""Words of a sheet's keys that keep a letter the normalization does not fold ("ł"), by the form people write without it.

A query written "Pilsudskiego 19, Krakow" or "Piotrkowska 100, Lodz" carries no "ł", and the keys "piłsudskiego" and
"łodz" are not reached. The engine rewrites such a query word to the sheet's word before it reads the query (rules class
29, `unfolded_words`; query.rs restore_unfolded_words). A folded form that is itself a word of the sheet keys is left
out: there the written word may be meant as it stands.

    python3 prep/unfolded_words.py data/build_pl.csv.gz eval/reference/pl/unfolded_words.tsv.gz

Output: gzip TSV `<folded word>\\t<word of the sheet>`, sorted.
"""
import argparse
import gzip
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from duck import connect_tuned  # noqa: E402

LETTERS = {"ł": "l"}


def unfolded_words(con, build_csv: pathlib.Path) -> list[tuple[str, str]]:
    folded = "word"
    for letter, plain in LETTERS.items():
        folded = f"replace({folded}, '{letter}', '{plain}')"
    marked = " OR ".join(f"contains(word, '{letter}')" for letter in LETTERS)
    return con.execute(f"""
        WITH w AS (
            SELECT DISTINCT unnest(string_split(nom_voie_norm || ' ' || nom_commune_norm, ' ')) AS word
            FROM read_csv('{build_csv}', header = true, all_varchar = true)
        ),
        m AS (SELECT {folded} AS plain, word FROM w WHERE {marked})
        SELECT plain, min(word) FROM m
        WHERE plain NOT IN (SELECT word FROM w) AND length(plain) > 2
        GROUP BY plain HAVING count(DISTINCT word) = 1
        ORDER BY plain
    """).fetchall()


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("build_csv", type=pathlib.Path)
    ap.add_argument("out", type=pathlib.Path)
    args = ap.parse_args(argv)
    rows = unfolded_words(connect_tuned(), args.build_csv)
    body = "".join(f"{plain}\t{word}\n" for plain, word in rows).encode("utf-8")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes(gzip.compress(body, mtime=0))
    print(f"{args.out}: {len(rows)} words")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
