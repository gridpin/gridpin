# Low-memory multi-region truth corpus (schema 5)

Schema 5 replaces the geographically narrow direct-OSM component of schema 4.
It remains a descriptive, mixed-source benchmark: it is not eligible for a
winner headline. Each country contributes exactly 300 rows:

- 50 countrywide Overture Places rows;
- 84 direct OSM rows from a fixed major-metro window;
- 83 direct OSM rows from a fixed mid-city window;
- 83 direct OSM rows from a fixed rural window.

The sampling rule is fixed before looking at any geocoder result. A major
metro has a metro-area population of at least one million; a mid-city has
50,000 through 500,000 residents; a rural window is anchored by a settlement
with fewer than 20,000 residents. Here `rural` means a rural/small-town sampling
window; it is not a claim about any jurisdiction's legal status. Netherlands
therefore uses the official Groot-Amsterdam metro area rather than claiming
that Amsterdam municipality is a million-person city.

## Exact region catalog

Bounding boxes are inclusive and use `min_lon,min_lat,max_lon,max_lat`.

| Region id | Role | Fixed window | PBF pin | Quota |
|---|---|---|---|---:|
| `fr-paris-metro` | major metro | `[2.15,48.75,2.55,49.00]` | `fr-idf` | 84 |
| `fr-strasbourg-mid` | mid-city | `[7.60,48.45,7.90,48.70]` | `fr-alsace` | 83 |
| `fr-altkirch-rural` | rural | `[7.00,47.40,7.50,47.75]` | `fr-alsace` | 83 |
| `it-rome-metro` | major metro | `[12.25,41.70,12.75,42.10]` | `it-centro` | 84 |
| `it-cagliari-mid` | mid-city | `[8.95,39.15,9.25,39.35]` | `it-isole` | 83 |
| `it-ghilarza-rural` | rural | `[8.45,39.85,9.05,40.35]` | `it-isole` | 83 |
| `nl-amsterdam-metro` | major metro | `[4.70,52.25,5.05,52.50]` | `nl-noord-holland` | 84 |
| `nl-assen-mid` | mid-city | `[6.45,52.93,6.65,53.08]` | `nl-drenthe` | 83 |
| `nl-westerbork-rural` | rural | `[6.45,52.65,7.05,52.90]` | `nl-drenthe` | 83 |
| `rs-belgrade-metro` | major metro | `[20.25,44.65,20.65,44.95]` | `rs-serbia` | 84 |
| `rs-novi-sad-mid` | mid-city | `[19.65,45.15,20.05,45.40]` | `rs-serbia` | 83 |
| `rs-backa-topola-rural` | rural | `[19.30,45.55,20.15,46.05]` | `rs-serbia` | 83 |

Wide rural windows additionally exclude fixed urban rectangles before
streaming: the Mulhouse edge from Altkirch, Oristano from Ghilarza, and
Hoogeveen and Emmen from Westerbork. These masks are
part of `region_catalog`, are applied in SQL, and are rechecked on every row.
They prevent address density in a neighboring mid-size city from silently
turning a rural stratum into another city stratum.

Direct-region selection disables the schema-4 municipality cap because a
fixed city stratum is intentionally allowed to come from one municipality.
It retains street, category and network caps. The 0.01-degree cell cap is
five for direct strata (at least 17 occupied cells for an 83-row quota), while
countrywide Overture keeps the stricter cap of three. The split is explicit in
the manifest and was required for the sparse direct-node coverage in Novi Sad;
the original, narrower Novi Sad window remains unchanged.

## Population and selection evidence

Population is evidence for the anchor/role, not a claim that every coordinate
in the rectangular window belongs to the named administrative territory.
Values, territory codes, dates and source URLs are retained verbatim in each
manifest region entry; the evidence set was checked on 2026-08-02.

| Region | Official territory and value | Primary evidence |
|---|---|---|
| Paris | INSEE `COM-75056`, municipality, 2,113,705 (2022) | [INSEE RP2022](https://www.insee.fr/en/statistiques/8588289?geo=COM-75056) |
| Strasbourg | INSEE `COM-67482`, municipality, 291,709 (2022) | [INSEE reference populations](https://www.insee.fr/fr/statistiques/8309996) |
| Altkirch | INSEE `COM-68004`, municipality anchor, 5,659 (2019) | [INSEE RP2019](https://www.insee.fr/fr/statistiques/6455183?geo=COM-68004) |
| Rome | ISTAT `058091`, municipality, 2,747,290 (2024) | [ISTAT 2024 census](https://www.istat.it/comunicato-stampa/censimento-e-dinamica-della-popolazione-anno-2024/) |
| Cagliari | ISTAT `092009`, municipality, 147,411 (2023) | [ISTAT Sardegna 2023](https://www.istat.it/wp-content/uploads/2025/04/Censimento-permanente-popolazione_Anno-2023_Sardegna.pdf) |
| Ghilarza | ISTAT `095021`, municipality anchor, 4,175 (2023) | [ISTAT census data portal](https://www.istat.it/notizia/popolazione-censuaria/) |
| Amsterdam | CBS `CR23`, Groot-Amsterdam COROP metro area, 1,480,814 (2025) | [CBS regional population](https://www.cbs.nl/en-gb/figures/detail/37259eng) |
| Assen | CBS `GM0106`, municipality, 70,392 (2025) | [CBS Areas in the Netherlands 2025](https://www.cbs.nl/nl-nl/cijfers/detail/86059NED) |
| Westerbork | CBS population core, 4,710 (2021) | [CBS population cores study](https://www.cbs.nl/nl-nl/longread/statistische-trends/2025/bevolkingsontwikkeling-van-bevolkingskernen-tussen-2011-en-2021?onepage=true) |
| Belgrade | SORS settlement, 1,197,714 (2022) | [SORS settlement workbook](https://popis2022.stat.gov.rs/media/31355/0_ukupan-broj-stanovnika-naselja.xlsx) |
| Novi Sad | SORS settlement, 260,438 (2022) | [same SORS workbook](https://popis2022.stat.gov.rs/media/31355/0_ukupan-broj-stanovnika-naselja.xlsx) |
| Bačka Topola | SORS settlement anchor, 11,930 (2022) | [same SORS workbook](https://popis2022.stat.gov.rs/media/31355/0_ukupan-broj-stanovnika-naselja.xlsx) |

The retained SORS workbook is 231,325 bytes with SHA-256
`5b1498923b90ec9930485ac3115cf10ad8f5ccf988840807d87ae70a58f57dc2`.

All seven PBFs have snapshot timestamp `2026-07-01T20:22:00Z`.

| Pin | File; bytes | SHA-256 | Replication sequence |
|---|---|---|---:|
| `fr-idf` | `ile-de-france-260701.osm.pbf`; 334789593 | `8cc2d3af326222a013eab1141ca4c388944893c918011a1930d7aa053045de1e` | 4833 |
| `fr-alsace` | `alsace-260701.osm.pbf`; 129643154 | `f8a63f9a31864821a16fa1fd1fd2626a587c4ea2d780a2d863bfa361d19bfaa7` | 4830 |
| `it-centro` | `centro-260701.osm.pbf`; 379876352 | `2c84214c99b21a2d89cf0b6479a248d3bab263751041c7f1c51cd2cffc55ffae` | 3893 |
| `it-isole` | `isole-260701.osm.pbf`; 212733976 | `b820ee216ef76b326bf1306ea946abfbfe58bdf08db9147289cf556d22665e88` | 3893 |
| `nl-noord-holland` | `noord-holland-260701.osm.pbf`; 187808082 | `6a757482b385e576d32abe4d4b77f8dfcb69f59d1f1fe0b220da373a9467baf3` | 2774 |
| `nl-drenthe` | `drenthe-260701.osm.pbf`; 62699767 | `2814290012b08420820e2ef47373156707128de68a2b15783302e8a6feafa326` | 2774 |
| `rs-serbia` | `serbia-260701.osm.pbf`; 236966213 | `0d5e526a7411e6a0dd7400bf188392d79de17477bd0612dee216a5a255fb83d0` | 4835 |

The script also verifies the exact public Geofabrik URL, replication base URL,
timestamp, sequence, file size and SHA-256 before extraction.

## Why extraction is split

Each `extract-region` invocation opens exactly one PBF in a fresh process.
DuckDB is fixed to one thread, a 768 MB memory limit, a 1 GB external temporary
directory limit, and 512-row fetches. The SQL stream has no full-stream
`ORDER BY`. Python retains at most `region quota * 64` candidates in a
deterministic heap (5,376 for the 84-row quota, 5,312 otherwise).

A global nonblocking `fcntl.flock` at
`tempfile.gettempdir()/gridpin-schema-v5-extract.lock` makes accidental
parallel extraction fail immediately even when the commands name different
outputs. The lock inode is retained; lock ownership is released by closing the
descriptor, including process exit, so a crashed process leaves no stale
logical lock to delete.

The heap does not weaken collision detection. Every valid OSM row in the
window is compared during the full stream with normalized-query and
rounded-coordinate maps built from that row's country-filtered subset of the
5,115 retained Overture candidates.
Colliding OSM rows never enter the heap; every colliding Overture record id is
stored in the fragment manifest. Assembly unions those ids and excludes both
sides before final selection and global deduplication.

Assembly reads one bounded fragment at a time. In memory it holds the retained
Overture candidates, at most one roughly 5.4k-row fragment, and the at-most
1,200 selected rows. It enforces engine-blind `outside_chain` minima of 25 FR,
20 IT, 10 NL and 25 RS (80 total).

The old country-pool municipality cap is intentionally disabled inside each
fixed direct-OSM stratum: a valid city window may contain only one
`addr:municipality`, so a cap of 12 would make an 84-row metro quota
mathematically impossible. Street (2), 0.01-degree cell (5), category (24),
and network (9) caps remain active. Countrywide Overture selection retains the
original full cap set, including its stricter cell cap of 3.

## Exact offline commands

Run from `code/`. Inputs must already exist; these commands do not download
anything. Do not run region commands in parallel. Output and temporary paths
must be new because publication is no-replace.

```bash
# Fill these in for your environment; nothing here is downloaded.
#   SOURCES  the directory holding the seven pinned *.osm.pbf files from the
#            "PBF pin" table above (each is re-verified by size and SHA-256).
#   ACQ      the retained Overture Places acquisition manifest (see
#            OVERTURE_PLACES_CORPUS.md).
#   WORK     a fresh, empty output directory for fragments and the corpus.
#   TMP      a fresh scratch directory for DuckDB temporary spill.
PY=.venv-py/bin/python
SOURCES=path/to/pinned-pbf-sources
ACQ=path/to/overture-places.acquisition.jsonl
WORK=path/to/schema-v5-work
TMP=path/to/schema-v5-duckdb-temp
AT=2026-08-02T00:00:00Z

"$PY" examples/multiregion_truth_corpus.py extract-region --region-id fr-paris-metro --osm "$SOURCES/ile-de-france-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/fr-paris-metro.candidates.jsonl" --duckdb-temp-directory "$TMP/fr-paris-metro" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id fr-strasbourg-mid --osm "$SOURCES/alsace-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/fr-strasbourg-mid.candidates.jsonl" --duckdb-temp-directory "$TMP/fr-strasbourg-mid" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id fr-altkirch-rural --osm "$SOURCES/alsace-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/fr-altkirch-rural.candidates.jsonl" --duckdb-temp-directory "$TMP/fr-altkirch-rural" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id it-rome-metro --osm "$SOURCES/centro-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/it-rome-metro.candidates.jsonl" --duckdb-temp-directory "$TMP/it-rome-metro" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id it-cagliari-mid --osm "$SOURCES/isole-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/it-cagliari-mid.candidates.jsonl" --duckdb-temp-directory "$TMP/it-cagliari-mid" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id it-ghilarza-rural --osm "$SOURCES/isole-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/it-ghilarza-rural.candidates.jsonl" --duckdb-temp-directory "$TMP/it-ghilarza-rural" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id nl-amsterdam-metro --osm "$SOURCES/noord-holland-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/nl-amsterdam-metro.candidates.jsonl" --duckdb-temp-directory "$TMP/nl-amsterdam-metro" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id nl-assen-mid --osm "$SOURCES/drenthe-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/nl-assen-mid.candidates.jsonl" --duckdb-temp-directory "$TMP/nl-assen-mid" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id nl-westerbork-rural --osm "$SOURCES/drenthe-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/nl-westerbork-rural.candidates.jsonl" --duckdb-temp-directory "$TMP/nl-westerbork-rural" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id rs-belgrade-metro --osm "$SOURCES/serbia-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/rs-belgrade-metro.candidates.jsonl" --duckdb-temp-directory "$TMP/rs-belgrade-metro" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id rs-novi-sad-mid --osm "$SOURCES/serbia-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/rs-novi-sad-mid.candidates.jsonl" --duckdb-temp-directory "$TMP/rs-novi-sad-mid" --assembled-at "$AT"
"$PY" examples/multiregion_truth_corpus.py extract-region --region-id rs-backa-topola-rural --osm "$SOURCES/serbia-260701.osm.pbf" --overture-acquisition "$ACQ" --candidate-output "$WORK/rs-backa-topola-rural.candidates.jsonl" --duckdb-temp-directory "$TMP/rs-backa-topola-rural" --assembled-at "$AT"

"$PY" examples/multiregion_truth_corpus.py assemble --overture-acquisition "$ACQ" --region-fragments "$WORK" --assembled-at "$AT" --output "$WORK/hybrid-truth-corpus-v5.jsonl"
"$PY" examples/multiregion_benchmark.py validate --truth "$WORK/hybrid-truth-corpus-v5.jsonl"
```

Run the local GridPin measurement only after validation:

```bash
"$PY" examples/multiregion_benchmark.py run \
  --truth "$WORK/hybrid-truth-corpus-v5.jsonl" \
  --gridpin-bin gridpin/target/release/gridpin \
  --sheet FR=/absolute/path/france.bin \
  --sheet IT=/absolute/path/italy.bin \
  --sheet NL=/absolute/path/netherlands.bin \
  --sheet RS=/absolute/path/serbia.bin \
  --work "$WORK/gridpin-work" \
  --output "$WORK/gridpin-schema-v5-result.json" \
  --distance-m 300
```

The runner accepts no threshold above 300 m. It reports country totals,
direct-OSM scores for every region, within-country region spread, and Overture
countrywide scores separately. Both the score and top-level result contain
`interpretation: descriptive_only` and `headline_eligible: false`.

## Retained local schema-5 measurement

The completed 2026-08-02 local run is evidence that the recipe can produce and
score the full fixed corpus. The corpus and result remain outside Git. They are
cryptographically bound to these artifacts:

- corpus SHA-256: `b0493a46d12ebeda65b6c8020cd5594f3bf7f524c30b02f41d8c00e8e5f5b2f8`;
- corpus manifest SHA-256: `ed30a81dfbccb5ca1cf3f809d46cfff83daa38eea5415c6e3bb74639a6bfc8bc`;
- result SHA-256: `a76b07695b3f6d69f65a8d3884c3c0a672150c9401d09f31ebc0bc5c465d7df1`;
- measured GridPin binary SHA-256: `97bc2d5fdef92b28731137112bff4bc84c6686feaa63bcb1a1c081d6512255f7`.

At hit@1 within 300 m, the mixed 1,200-row descriptive total was
1,045/1,200 (87.083%). The direct-OSM component was 890/1,000 (89.000%);
the separately reported countrywide Overture component was 155/200 (77.500%).

| Country | Mixed descriptive | Overture countrywide |
|---|---:|---:|
| FR | 259/300 (86.333%) | 43/50 (86.000%) |
| IT | 231/300 (77.000%) | 28/50 (56.000%) |
| NL | 292/300 (97.333%) | 47/50 (94.000%) |
| RS | 263/300 (87.667%) | 37/50 (74.000%) |

| Direct-OSM region | Role | Result |
|---|---|---:|
| Paris | major metro | 79/84 (94.048%) |
| Strasbourg | mid-city | 68/83 (81.928%) |
| Altkirch | rural/small-town | 69/83 (83.133%) |
| Rome | major metro | 78/84 (92.857%) |
| Cagliari | mid-city | 65/83 (78.313%) |
| Ghilarza | rural/small-town | 60/83 (72.289%) |
| Amsterdam | major metro | 82/84 (97.619%) |
| Assen | mid-city | 81/83 (97.590%) |
| Westerbork | rural/small-town | 82/83 (98.795%) |
| Belgrade | major metro | 78/84 (92.857%) |
| Novi Sad | mid-city | 78/83 (93.976%) |
| Bačka Topola | rural/small-town | 70/83 (84.337%) |

These values are diagnostic only. The direct truth is OSM-derived, source
relationships to the GridPin sheets are not proven independent, no competitor
was run on this exact corpus, and the mixed total must not be presented as a
winner or release headline.
