# Table passport (`*.table.json`)

A table passport describes a table of statistics published by area or by grid cell, so that `gridpin attach`
(and `gridpin-pro batch --table`) can join it to geocoded rows by a code. The passport lies beside the table file.
The JSON Schema is [`table-passport.schema.json`](table-passport.schema.json).

```json
{
  "table": "acs5_2024_tract",
  "file": "acs5_2024_tract.csv.gz",
  "prefix": "acs",
  "key": {"column": "GEOID", "kind": "us_tract", "vintage": "2020", "width": 11, "grain": "fine"},
  "take": {
    "median_household_income": {"as": "median_household_income", "unit": "USD",
                                "moe": "median_household_income_moe", "missing": ["-666666666"]}
  },
  "edition": "ACS 5-year 2020-2024",
  "publisher": "U.S. Census Bureau",
  "source_url": "https://www2.census.gov/programs-surveys/acs/summary_file/2024/table-based-SF/data/5YRData/",
  "read": "2026-11-19",
  "licence": "U.S. federal government work (17 U.S.C. 105)",
  "coverage": "complete",
  "max_absent_share": 0.02,
  "caveat": "A value of the census tract the address lies in, not of the household or the person at the address."
}
```

## Fields

| Field | Required | Meaning |
|---|---|---|
| `table` | yes | a name for messages and reports |
| `file` | yes | the table file, relative to the passport: CSV or TSV, optionally gzip (`.gz`) |
| `delimiter` | no | a separator other than the one of the extension (`;`) |
| `prefix` | yes | output columns are `gp_<prefix>_<as>` |
| `key.column` | yes | the code column of the table |
| `key.kind` | yes | what the code is (`us_tract`, `au_sa2`, …) or a key computed from the row's point: `h3:<level>`, `geohash:<length>`, `cell:eu1km`, `cell:de100m`, `cell:jp1`, `cell:jp2`, `cell:jp3` |
| `key.vintage` | for codes of boundaries | the year the boundaries were drawn; `null` for a computed key |
| `key.width` | no | codes of only digits shorter than this are padded with zeros on the left |
| `key.grain` | no | `fine` (default: tract, small area, grid cell) or `coarse` (county, region) |
| `take` | yes | source column → `as` (output name), `unit`, `moe` (its margin-of-error column), `missing` (the publisher's marks of a value that is not given) |
| `valid` | no | `from` / `to` for the whole table, or `from_column` / `to_column` per row (one key may have several rows whose dates do not overlap); both days are inside; a table with dates needs `--as-of` |
| `edition`, `publisher`, `licence`, `caveat` | yes | printed after every run |
| `source_url`, `read`, `attribution` | no | where the table came from, the day its terms were read, the attribution line |
| `coverage` | no | `complete` when every code of its kind is in the table, `partial` otherwise |
| `max_absent_share` | no | with `complete`: the share of looked-up codes the table may miss (default 0.02) |

## What is refused

1. **Codes of another year of boundaries.** A tract code of 2010 names another area in the 2020 boundaries. A table
   with a `vintage` needs `--key-vintage` with the same year (or, in one pass, a layer whose passport names it).
2. **Many absent codes in a complete table.** More than `max_absent_share` of the codes missing means codes of
   another year or kind; `--allow-absent` writes the rows anyway, with a warning.
3. **A small area for a point that does not fix it.** A value of a tract or a grid cell is attached only to a house
   point (`gp_key_basis` = `point`), a point on the street (`approximate`) or the input's own coordinates (`given`).
   The centre of a town or of a postcode gets no key (`point_too_coarse`), nor does a weak answer (`weak_answer`).
   Rows that carry `gp_lat`, `gp_lon` without `gp_precision` or `gp_point_method` cannot be judged
   (`basis_unknown`).

## Output

Per row: the taken values (and `_moe`), `gp_<prefix>_status` — `matched`, `key_absent` (the table does not hold the
code), `no_key` (the row has no code, or its point does not fix the area), `suppressed` (the publisher gives no
value), `out_of_dates` — `gp_<prefix>_edition`, and `gp_key_basis`. Counts, edition, licence and the caveat go to
standard error and, with `--report`, to a JSON file.

A layer name of the form `code|label` gives its code to the join.

A value of a table is a value of the area the address lies in, not of the household or the person there.
