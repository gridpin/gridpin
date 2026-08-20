# Public quality benchmark

**Implementation status (2026-08-04): a fixed schema-v4 hybrid corpus, its
manifest and diagnostics, and one complete run measuring GridPin, Photon and
Nominatim together are retained.** That run's `not_run` record is empty: every
service was measured, on one truth corpus, one metric, one runner and one set
of GridPin artifacts. No value from another corpus is substituted. Earlier
partial runs are kept as history and are marked as superseded. The release-asset route below is available from release `v0.1.0`.

## Intended metric and truth contract

- Countries: France (`FR`), Italy (`IT`), the Netherlands (`NL`), and Serbia
  (`RS`).
- Retained size: exactly 300 unique rows per country, including at least 150
  `unknown_lineage` rows per country, after record id, normalized full-query and
  rounded-coordinate deduplication. Each country contains 50 Overture Places
  rows and 250 direct OSM address nodes.
- Query completeness: each row must contain a numbered street address,
  municipality and country; postcode is retained and included when the source
  provides one. The exact same composed query is sent to GridPin and every
  selected competitor.
- Metric: hit@1 within 300 metres; an empty answer is a miss. The 300 m threshold
  tolerates building-centre and entrance-point differences without turning a
  city-level answer into a hit.
- Provenance: every row carries its public source URL, source release, retrieval
  time and license plus coordinate-specific source, record id, retrieval time,
  license and evidence. `same_export_as_indexed_sheet` must be exactly `false`.
- Lineage: every row is classified as `outside_chain`, `common_upstream` or
  `unknown_lineage`. The result reports overall, country, lineage and
  country-by-lineage slices; an empty slice is `null`, never an invented zero.

The fixed recipe combines two inputs. It reads the pinned Overture release
`2026-06-17.0`, `theme=places`, rather than the `theme=addresses` exports used
to build several GridPin sheets, and it reads exact Geofabrik PBF snapshots for
Alsace, Isole, Drenthe, and Serbia at `2026-07-01T20:22:00Z`. Every input byte
count and SHA-256 is pinned. The offline assembler disables extension
installation and network reads, excludes all cross-source query/coordinate
collisions, and applies fixed municipality, street, cell, category, and network
caps before selection.

The Overture separation prevents direct same-export truth but does not prove
that providers have no common upstream. The extractor preserves every effective
coordinate `SourceItem`, classifies only directly evidenced Foursquare rows as
`outside_chain`, records known sheet ancestors as `common_upstream`, and sends
OSM, unfamiliar, missing-license, or ambiguous rows to `unknown_lineage`.
Direct OSM rows are always `unknown_lineage`. See the complete reusable
[public truth-corpus recipe](OVERTURE_PLACES_CORPUS.md), including the evidence
and separately reviewed route needed for a future private 1,000-or-more-row
corpus.

The retained corpus SHA-256 is
`d62033a60c434fe1d9a8937681cb014e8d2f75bccb934df465a791dda227433f`;
its manifest SHA-256 is
`9e7fcd2c2579da51a56b66c9019c7b30f97d46caf9fc88661d8386957d442be4`.
It contains 72 `outside_chain`, zero `common_upstream`, and 1,128
`unknown_lineage` rows. The manifest binds every country/source/lineage matrix,
snapshot, license, acquisition hash, and recipe hash. Empty, truncated,
duplicate, out-of-extent, incomplete, or provenance-invalid corpora fail before
a score is calculated.

Because 1,000 of 1,200 rows are OSM-derived, Nominatim and Photon each have a
disclosed `same_dataset_family` relationship to those rows: both search
OpenStreetMap-derived data. GridPin's relationship to the OSM inputs is
conservatively `unknown`. Every source slice and the mixed overall score has
`headline_eligible=false`; the values are descriptive measurements, not an
independent winner claim. The mixed total in particular is dominated by the
OSM-derived five sixths of the corpus and must not be read as a ranking.

| Fixed truth input | Snapshot / sequence | Bytes | SHA-256 |
|---|---|---:|---|
| Overture Places acquisition | `2026-06-17.0` | recorded in its acquisition manifest | `1697a275dfca8e1b5bb3577dd65200d9b6ed3f335b02ec73a748eac2d94707b9` |
| Geofabrik Alsace PBF | `2026-07-01T20:22:00Z` / 4830 | 129,643,154 | `f8a63f9a31864821a16fa1fd1fd2626a587c4ea2d780a2d863bfa361d19bfaa7` |
| Geofabrik Isole PBF | `2026-07-01T20:22:00Z` / 3893 | 212,733,976 | `b820ee216ef76b326bf1306ea946abfbfe58bdf08db9147289cf556d22665e88` |
| Geofabrik Drenthe PBF | `2026-07-01T20:22:00Z` / 2774 | 62,699,767 | `2814290012b08420820e2ef47373156707128de68a2b15783302e8a6feafa326` |
| Geofabrik Serbia PBF | `2026-07-01T20:22:00Z` / 4835 | 236,966,213 | `0d5e526a7411e6a0dd7400bf188392d79de17477bd0612dee216a5a255fb83d0` |

The retained Overture acquisition manifest SHA-256 is
`ab9b3ea830189ac3d7eaa9d356035503a4ae1d8c698bdc5a74b752d5f8aece16`.
Alsace, Isole, and Drenthe are regional extracts, not national samples; only the
Serbia PBF is country-wide. Country labels identify the GridPin sheet and query
country, but these 300-row slices must not be generalized to all of France,
Italy, or the Netherlands. The source and municipality/cell caps reduce local
concentration without converting a regional input into national coverage.

## Retained pre-release result

The result was generated at `2026-08-01T23:55:14.102891+00:00`. Its SHA-256 is
`03da6894bffc41ffa8bd2de51d68ddd824f359a87a3a85dfede5e96f1ecd5645`;
the exact runner SHA-256 is
`72e425adb70ce2d670f84175479c4dbd6236f712f1c3c3d02682c6feb2733665`;
the GridPin binary SHA-256 is
`4584b46a5382babf2923005cf3a35b39dfd7ded077d79cb23e12ffe9fab6ad4b`.

| Country | GridPin hit@1 ≤300 m | Photon | Nominatim hit@1 ≤300 m |
|---|---:|---:|---:|
| FR | 256/300 — **85.333%** | not run | 261/300 — **87.000%** |
| IT | 216/300 — **72.000%** | not run | 248/300 — **82.667%** |
| NL | 290/300 — **96.667%** | not run | 292/300 — **97.333%** |
| RS | 262/300 — **87.333%** | not run | 274/300 — **91.333%** |
| Mixed total, descriptive only | 1,024/1,200 — **85.333%** | not run | 1,075/1,200 — **89.583%** |

Nominatim is higher in the mixed total and all four country rows. The result is
still not an independent winner ranking: 1,000 rows come directly from OSM,
which is Nominatim's dataset family. The source split makes the effect visible:

| Truth source | Cases | GridPin | Nominatim | Relationship note |
|---|---:|---:|---:|---|
| OSM / Geofabrik, all four extracts | 1,000 | 868 — **86.800%** | 943 — **94.300%** | Nominatim `same_dataset_family`; GridPin `unknown` |
| Overture Places | 200 | 156 — **78.000%** | 132 — **66.000%** | both relationships `unknown` |

Every source score and the mixed total has `headline_eligible=false`. The
Overture slice is not promoted to an independence claim either; `unknown` means
the relationship has not been established, not that independence was proved.

| Country | Lineage class | Cases | GridPin | Photon | Nominatim |
|---|---|---:|---:|---:|---:|
| FR | `outside_chain` | 27 | 25 — 92.593% | not run | 20 — 74.074% |
| FR | `common_upstream` | 0 | — | not run | — |
| FR | `unknown_lineage` | 273 | 231 — 84.615% | not run | 241 — 88.278% |
| IT | `outside_chain` | 5 | 1 — 20.000% | not run | 2 — 40.000% |
| IT | `common_upstream` | 0 | — | not run | — |
| IT | `unknown_lineage` | 295 | 215 — 72.881% | not run | 246 — 83.390% |
| NL | `outside_chain` | 1 | 1 — 100.000% | not run | 1 — 100.000% |
| NL | `common_upstream` | 0 | — | not run | — |
| NL | `unknown_lineage` | 299 | 289 — 96.656% | not run | 291 — 97.324% |
| RS | `outside_chain` | 39 | 31 — 79.487% | not run | 27 — 69.231% |
| RS | `common_upstream` | 0 | — | not run | — |
| RS | `unknown_lineage` | 261 | 231 — 88.506% | not run | 247 — 94.636% |

The `outside_chain` cells for IT and NL have only five and one case; their
percentages are displayed for completeness and are not stable estimates.
`common_upstream` is shown as an empty slice, never as 0%.

Each GridPin number above is bound to the following local release-candidate
sheet, not to an unspecified filename:

| Country | Sheet SHA-256 | Embedded `source_release` |
|---|---|---|
| FR | `faab5408afe68df1dadfd84ac06fbb3b644590dcf685046ad9171b99c7a41d1a` | `2026-06-12` |
| IT | `e98f1d0fb9c83a7f1caf2b7aab546c5f4a4a3ed810fc211a53ed110eb7381988` | `2026-06-17.0` |
| NL | `ba8b0462cd0af7ee7e2aaa532c8345b4bc31422bbc12f281ab6ca2936597927c` | `2026-06-17.0` |
| RS | `65c0c1a2b81f7636284e1d4ac0dec5e80dc705e743ebe965d677a64c9498348f` | `2026-06-17.0` |

Nominatim identity was
`nominatim@https://nominatim.openstreetmap.org/search`, software version 5.3.0,
database version `5.3.99-1`. The pre-run status evidence SHA-256 is
`63aa24f31e739af01e25eb55782cbe3f20a6f04d747844d5bcbe60c8995deeeb`;
the database reported data time `2026-08-01T23:15:36+00:00`. The sequential
1.1-second run spans `23:22:23` through `23:53:53` UTC and retains all 1,200 raw
responses in a cache with SHA-256
`ccb85a9374514280fbc9e652f14bcfc015246a5d352d46998394c9fb648de322`.
The public service is not an isolated immutable database snapshot; the sealed
cache is the reproducible evidence for these numbers.

In this 2026-08-01 run Photon has the explicit status `not_run`, and no
historical Photon value was substituted into it. That gap has since been closed
by a separate run against a self-hosted database, recorded in the next section.

## Retained three-service run

The 2026-08-01 result above left Photon `not_run` because no four-country
database existed locally. One was then built, and all three services were
measured **in a single run** on the same fixed truth corpus, the same
`hit@1 ≤300 m` metric, the same runner and the same GridPin binary and sheets.
Its `not_run` record is empty: no service is unmeasured. This is the result to
read; the 2026-08-01 result stays above as history, and an intermediate
GridPin/Photon-only run (result SHA-256
`7af980f18817bf1b749f971267c72f6f6e4e65e06777270969ebe5da2c5ea202`) is retained
but superseded by this one.

The result was generated at `2026-08-04T11:16:22.424467+00:00`. Its SHA-256 is
`4158865253d072f40bc1f2cacfe1c7f3380d11e84c54356b527ab12ad7851ac9`; the runner
SHA-256 is unchanged at
`72e425adb70ce2d670f84175479c4dbd6236f712f1c3c3d02682c6feb2733665`; the GridPin
binary SHA-256 is
`c67ea070f77f87440edd310c6eb1502939b23caa4c62bf225f8817ed14f56b24`.

GridPin and Photon reproduced their earlier per-country and per-source numbers
exactly, across a GridPin rebuild between the two runs. Nominatim did not: its
mixed total is unchanged at 89.583%, but IT moved 82.667% → 83.000%, RS
91.333% → 91.000%, the OSM slice 943 → 942 and the Overture slice 132 → 133.
That is the expected signature of a live public service whose database advanced
between 2026-08-01 and 2026-08-04, not of a defect. Only the two self-hosted
engines are byte-reproducible here.

| Country | Sheet SHA-256 | Embedded `source_release` |
|---|---|---|
| FR | `f7278398c1a63308ea3dc69e5cda2f779250716fc231b2122d6ee890da237839` | `2026-06-12` |
| IT | `37346d884e551189e91f0453d4aad7aafe211430e62b1e9054141081488b021c` | `2026-06-17.0` |
| NL | `d5205f8e323eeda309f3d411a3efcd45c259005d2da25fef33998bf9ca9d28e7` | `2026-06-17.0` |
| RS | `9e358bd6a940b290e01cd52081e476e2799513f16dd934dc6a5bc89ba4fb5388` | `2026-06-17.0` |

Photon identity was `photon@http://127.0.0.1:2323/api/`, software version
`1.2.0`, git commit `c6dc628a`, reported `import_date` `2026-08-01T23:00:21Z`.
Its status evidence SHA-256 is
`766dcc51bfafa9074a3d8fe4d753aa3abf4165ce3bdb532aef2719fae64446b9`. The instance
is self-hosted and `public_demo` is `false`; the canonical public demo was not
used. Photon's 1,200 answers were served from the complete retained response
cache of the earlier run, which is valid only because corpus, endpoint, service
version and status-evidence hash were all identical; the instance was therefore
not queried again.

Nominatim identity was
`nominatim@https://nominatim.openstreetmap.org/search`, software version
`5.3.0`, database version `5.3.99-1`, reported `data_updated`
`2026-08-04T10:34:12+00:00`. Its status evidence SHA-256 is
`330a5d5e14ae3206685e5aecc7d8adcf0e889b6b887e7666b17cf11fc043d71f`. This was a
deliberate one-time run against the public service, at a 1.5-second sequential
pause — 0.52 requests per second measured end to end, below the service's
1 request/second ceiling and slower than the 1.1-second floor the runner
enforces. It spans `10:36:36` through `11:16:21` UTC, sent an identifying
contact User-Agent, and completed with no rejected, throttled or failed
request. All 1,200 raw responses are retained in a cache with SHA-256
`9a071a3e12c45e2f9d550b2a35db2d7ac4fca07b075f38f49983af959b19789c`; the reused
Photon cache has SHA-256
`83b99805cddd0aabafe4847830bf430b9cc5ee7e478068ffb087a5fd6b994d83`.
The public service is not an isolated immutable database snapshot; the sealed
cache is the reproducible evidence for these numbers.

The database was built by streaming the published planet dump
`photon-dump-planet-1.0-latest.jsonl.zst` (dump format `1.0.0-4`,
`data_timestamp` `2026-08-01T23:00:21Z`) through Photon's own importer with
`-country-codes fr,it,nl,rs`, producing one instance holding all four countries
and therefore one single import date and one status identity. It contains
42,249,775 documents. Import languages were set to `en,fr,it,nl,sr`: the
runner queries each country in its own language, and Photon's default import
language set (`en,de,fr,it`) covers neither Dutch nor Serbian, so accepting the
default would have understated Photon through configuration rather than through
capability.

Three disclosures that work against GridPin are stated rather than omitted. Both
competitors hold **newer** data than the compared sheets: the Photon dump is
timestamped `2026-08-01` and Nominatim reported `data_updated` `2026-08-04`,
against sheet `source_release` `2026-06-12`/`2026-06-17.0`. Photon additionally
answered from a warm local instance with no network latency and no usage policy
applied. None of these effects was corrected for.

| Country | GridPin hit@1 ≤300 m | Photon hit@1 ≤300 m | Nominatim hit@1 ≤300 m |
|---|---:|---:|---:|
| FR | 256/300 — **85.333%** | 274/300 — **91.333%** | 261/300 — **87.000%** |
| IT | 218/300 — **72.667%** | 269/300 — **89.667%** | 249/300 — **83.000%** |
| NL | 290/300 — **96.667%** | 292/300 — **97.333%** | 292/300 — **97.333%** |
| RS | 262/300 — **87.333%** | 257/300 — **85.667%** | 273/300 — **91.000%** |
| Mixed total, descriptive only | 1,026/1,200 — **85.500%** | 1,092/1,200 — **91.000%** | 1,075/1,200 — **89.583%** |

Photon leads the mixed total and the French and Italian rows; in the Netherlands
Photon and Nominatim tie at the top; in Serbia GridPin is above Photon but below
Nominatim. There is no country row where GridPin leads both competitors. This is
not an independent winner ranking. Five sixths of the corpus is OSM-derived and
both competitors search OSM-derived data, so the mixed total is measured on
their own dataset family. The source split separates the effect:

| Truth source | Cases | GridPin | Photon | Nominatim | Relationship note |
|---|---:|---:|---:|---:|---|
| OSM / Geofabrik, all four extracts | 1,000 | 869 — **86.900%** | 973 — **97.300%** | 942 — **94.200%** | both competitors `same_dataset_family`; GridPin `unknown` |
| Overture Places | 200 | 157 — **78.500%** | 119 — **59.500%** | 133 — **66.500%** | all relationships `unknown` |

**The ranking inverts between the two slices.** On the 1,000 rows taken from
OSM the order is Photon 97.300%, Nominatim 94.200%, GridPin 86.900%. On the 200
Overture Places rows, whose relationship to all three engines is `unknown`, the
descriptive order is exactly reversed: GridPin 78.500%, Nominatim 66.500%,
Photon 59.500%. Every one of these cells has `headline_eligible=false`,
and the Overture slice is still not promoted to an independence claim:
`unknown` means the relationship has not been established, not that
independence was proved.

| Country | Lineage class | Cases | GridPin | Photon | Nominatim |
|---|---|---:|---:|---:|---:|
| FR | `outside_chain` | 27 | 25 — 92.593% | 20 — 74.074% | 20 — 74.074% |
| FR | `common_upstream` | 0 | — | — | — |
| FR | `unknown_lineage` | 273 | 231 — 84.615% | 254 — 93.040% | 241 — 88.278% |
| IT | `outside_chain` | 5 | 1 — 20.000% | 2 — 40.000% | 2 — 40.000% |
| IT | `common_upstream` | 0 | — | — | — |
| IT | `unknown_lineage` | 295 | 217 — 73.559% | 267 — 90.508% | 247 — 83.729% |
| NL | `outside_chain` | 1 | 1 — 100.000% | 1 — 100.000% | 1 — 100.000% |
| NL | `common_upstream` | 0 | — | — | — |
| NL | `unknown_lineage` | 299 | 289 — 96.656% | 291 — 97.324% | 291 — 97.324% |
| RS | `outside_chain` | 39 | 31 — 79.487% | 13 — 33.333% | 27 — 69.231% |
| RS | `common_upstream` | 0 | — | — | — |
| RS | `unknown_lineage` | 261 | 231 — 88.506% | 244 — 93.487% | 246 — 94.253% |

Across all four countries the `outside_chain` class totals 72 cases: GridPin 58
— 80.556%, Nominatim 50 — 69.444%, Photon 36 — 50.000%. It carries the same
ordering as the Overture slice, on a differently defined cut. Its Serbian cell
is the widest single gap in the run, 79.487% against Photon's 33.333%, on 39
cases. The IT and NL cells hold only five and one case and are displayed for
completeness, not as stable estimates.

Empty answers, counted as misses throughout: GridPin 77, Photon 32,
Nominatim 86.

## Retained strictly independent run

The corpus above is five-sixths OSM-derived, so no cell in it is
`headline_eligible`. A second, deliberately small corpus was therefore built
from the only coordinate provider whose independence from both OSM and the
indexed national registries is established: Foursquare-sourced Overture Places
rows. It holds **230 rows — FR 43, IT 129, NL 17, RS 41 — every one of them
`outside_chain`**, and it is the exact cap-respecting maximum available from the
pinned acquisition, not a sample of it. Result SHA-256
`03a227a79967c1f41bb49a9f4c2c5228e69e1b8c15e1d80c2f9a2f70ee040d28`; corpus
SHA-256
`0489a8e39b936f97a004bf9b7d028705d2afea9a572a713d85b568025532b6ce`; generated at
`2026-08-05T05:18:47.930820+00:00`. Its `not_run` record is empty.

Because every query is sent to all three engines, the observations are paired
and the comparison is an exact two-sided McNemar test on discordant pairs, not a
test of two independent proportions.

| Engine | hit@1 ≤300 m | 95% Wilson CI | vs GridPin | discordant | exact McNemar |
|---|---:|---|---:|---:|---:|
| GridPin | 140/230 — **60.870%** | 54.4–66.9 | — | — | — |
| Nominatim | 128/230 — **55.652%** | 49.2–61.9 | −5.217 pp | 24 / 12 | p = 0.0652 |
| Photon | 116/230 — **50.435%** | 44.0–56.8 | −10.435 pp | 40 / 16 | p = 0.0018 |

**The two comparisons do not carry the same weight, and the difference is
stated rather than blurred.** Against Photon the lead is significant at the
conventional 5% level. Against Nominatim it is not: p = 0.0652 means the
direction favours GridPin while the evidence remains insufficient. No claim of
superiority over Nominatim is made from this run.

| Country | Cases | GridPin | Photon | Nominatim |
|---|---:|---:|---:|---:|
| FR | 43 | **86.047%** | 74.419% | 72.093% |
| IT | 129 | **46.512%** | 45.736% | 43.411% |
| NL | 17 | 64.706% | 64.706% | 64.706% |
| RS | 41 | **78.049%** | 34.146% | 73.171% |

Country rows are descriptive only. NL holds 17 cases and supports no country
claim at all; the inferential target is the pooled 230-row sample fixed before
the run.

**A caution about the earlier 72-row slice.** The same lineage class inside the
1,200-row corpus gave GridPin 80.556% and a far larger lead. That number and
this one are not in conflict, and neither is "the" independent accuracy: they
differ almost entirely by country mix. The 72-row slice was 54% Serbia, where
GridPin is strong, and 7% Italy, where it is weakest; this corpus is 18% Serbia
and 56% Italy. Applying this run's per-country rates to the old composition
reproduces 78.7% against the 80.556% actually observed — a gap within noise at
n=72. The per-country rates are the stable quantity; any pooled figure is an
artefact of the mix behind it.

**Limits that survive this run.** The independent truth comes from a single
upstream provider, so "independent of OSM and of the indexed registries" is
established while "independent" in a stronger sense is not. Every row is a POI,
whereas the product indexes national address registries — this corpus measures
an adjacent capability, not the main one. Photon answered from a warm local
instance, and both competitors hold data newer than the sheets. Photon's 230
answers were served from the retained cache of the earlier run under an
identical corpus, endpoint, version and status-evidence hash; Nominatim was
queried live at a 1.5-second sequential pause with an identifying contact
User-Agent and completed with no rejected or throttled request. Retained
caches: Nominatim `55d9de3623b51dc7…`, Photon `acea0366741cb75d…`, 230 rows
each.

## geocoder-tester interoperability

Start the dependency-free adapter for one country sheet:

```sh
make http INDEX=/absolute/path/france.bin COUNTRY=FR
```

It binds to `127.0.0.1:2322` by default and exposes `/api`, `/api/`, and the
exact path `/reverse` as Photon-style GeoJSON `FeatureCollection` endpoints.
Configure geocoder-tester with
`--api-url http://127.0.0.1:2322 --api-type photon`. The value passed to
`--api-url` must be the base origin without `/api`: the tested upstream
`PhotonApi` appends `/api` itself. Passing an endpoint ending in `/api/`
therefore requests `/api//api` and fails with a non-200 response. Photon format
was chosen because a GridPin hit maps directly to one GeoJSON feature while
retaining street, postcode, city and country code. The GridPin CLI exposes the
matched house number when a house was actually resolved: exact and near-snap
answers report the stored address (including its suffix), while interpolation
reports the requested interpolated address. The adapter forwards that value as
Photon `housenumber`; it does not reconstruct it from the query. Photon
`lat`/`lon` focus parameters are range-checked and handled by two complementary
layers. The adapter requests a bounded wider candidate list, forwards the point
to `gridpin query --near LAT,LON`, and re-ranks the returned pool by great-circle
distance while keeping the CLI's order among equally distant candidates. The
engine uses the same point conservatively: it can add exact same-name streets
from the local spatial grid when the global prefix cap omitted them, and it uses
distance only after every existing text/address-quality discriminator is equal.
The adapter still never invents a result; it only orders the engine's widened
pool. Mandatory fake-CLI observers prove both the exact `--near` argument and
the adapter re-ranking, while a real CLI integration test proves that the point
reaches the engine and can inject a local homonym.

A real upstream smoke run was retained on 2026-08-01. The tested
[geocoder-tester](https://github.com/geocoders/geocoder-tester) identity was Git
commit `5384d1534bc3c59e8d280be3d951a92356ce470b`; the project publishes no
release version or installable Python package. It ran under Python 3.10.9 with
the upstream direct pins pytest 9.1.1, geopy 2.5.0, requests 2.34.2, PyYAML
6.0.3 and Unidecode 1.4.0; the pytest session also identified pluggy 1.6.0.
From the upstream checkout, against an adapter bound to an otherwise unused
loopback port, the retained smoke transcript used this exact command (the
dedicated venv name is a local artifact):

```sh
../geocoder-tester-venv-5384d153/bin/python -m pytest \
  geocoder_tester/world/france/alsace/test_from_user_input.csv \
  --api-url http://127.0.0.1:24322 --api-type photon \
  --loose-compare -vv --tb=short
```

It collected the one official case and reported `1 passed in 0.48s`. This is a
forward-protocol smoke proof, not a claim that GridPin passes the full upstream
suite. Without upstream's documented `--loose-compare` option, the same request
was parsed successfully but failed the strict value comparison: the fixture
expected `Rue de l'École`, while the sheet returned `Rue de l’Ecole`
(`1 failed in 0.83s`).

The complete official Alsace numbered-address file was then run, not sampled:

```sh
../geocoder-tester-venv-5384d153/bin/python -m pytest \
  geocoder_tester/world/france/alsace/test_addresses.csv \
  --api-url http://127.0.0.1:24322 --api-type photon \
  --loose-compare --tb=no -q \
  --save-report ../geocoder-tester-alsace-20260801.report \
  --junitxml=../geocoder-tester-alsace-20260801.junit.xml
```

The original 2026-08-01 run collected 285 cases and reported **0 passed, 285
failed in 25.53s**. The
adapter returned consumable Photon-shaped GeoJSON and the first fixture matched
`Rue du Presbytère`, postcode `67130`, `Barembach`, and a nearby coordinate.
Every numbered fixture nevertheless asserts `properties.housenumber`; the
then-current GridPin CLI hit did not expose that value, so the adapter
deliberately did not invent it. The retained historical JUnit SHA-256 is
`5607bceacd51b6a3d37ec2021f5c52068b1c70e3dc3158d9cdfcb59278543f3b`;
the upstream report SHA-256 is
`2625c0ff95d31fef8f6a861ee5053bc704d003e43262b61ad63596dc986a8880`.

After the forward result began carrying the address actually used for the
returned point, the same upstream commit and all 285 cases were rerun on
2026-08-02: **173 passed, 112 failed in 346.628s**. Among 279 non-empty answers,
260 (93.19%) carried the exact requested house number; 19 carried a different
non-empty number and six cases were empty. The largest remaining failure class
is the proximity-biased variant: 77/95 fail, including 72 postcode-only
mismatches across the complete suite. Coordinates are not asserted by this
upstream CSV, so this is an interoperability/component comparison, not a
coordinate-accuracy benchmark. The new JUnit SHA-256 is
`86330fc6cf1499bcf9fcb0550c5746153e351a22c8df157467b202903608a834`;
the failure-report SHA-256 is
`4001a867dd37c9ec5758389f0dfaa3e61188f02576611d509115a1dd4b164611`.

The adapter-only focus baseline was later fixed at **208 passed / 77 failed**.
On 2026-08-07 the hybrid engine-plus-adapter implementation was run against the
same complete 285-case file and reached **230 passed / 55 failed**. Its failure
set is a strict subset of the baseline: 22 old failures closed and zero old
passes regressed. This is deliberately not a distance-weight tuning: the engine
only widens exact same-name homonyms and breaks otherwise equal-quality ties;
the shipped adapter still performs the final distance ordering over at most 100
candidates. A separate OLD/NEW comparison without `--near` covered 83 live
queries, all 1,128 structural cases and the 230-row external corpus — **1,441
queries, zero byte differences**. Retained evidence: baseline JUnit SHA-256
`fece3586a927f5ee51de7e8c71c0859bc48ca63c54b52b9a1316d571f2300620`;
hybrid JUnit SHA-256
`5fc6819d523ff654ad8398211aac4b23a9a99e7b954df5e754501e2144c59669`;
hybrid report SHA-256
`18e05a03b2a246336f53db2d7b9d56af484dc23e13631b80bcd62a84bb1907c4`;
tested binary SHA-256
`47a6ecf2cee6835dec7c7dd2fb331ebb5c8708f084e3b02f6bb5239b6e2103b8`.

The reverse endpoint was added and checked on 2026-08-06. It requires one
finite, in-range `lat`/`lon` pair; accepts the same optional `limit` and
validated-but-ignored `lang` contract as the forward endpoint; and accepts
`query_string_filter` only when its exact value is `osm_key:highway`. Unknown,
repeated, empty, or differently valued parameters fail closed with HTTP 400.
It invokes `gridpin reverse` under the same timeout and concurrency bound and
maps its rows through the same strict Photon feature mapper; engine-only
`distance_m` and `region` fields are not exposed.

The pinned upstream `world/test_reverse_curated_cities.csv` file contains one
header and 109 cases, but **zero cases in France**: 106 name German cities and
the other three name Vienna, Havana, and Minsk. Against the one-country
`france.bin` sheet the complete file therefore reported **0 passed, 109 failed
in 11.08s**, correctly classified as 0/0 in-scope French cases and 109
out-of-scope cases — not as a 0/109 reverse-quality score. All 109 requests
were consumed through the Photon reverse protocol without HTTP, JSON, or
adapter-parser errors. The retained JUnit SHA-256 is
`8c7467f74a158adcf8e5e7cfb394cb647c2939f7e8f2932707b4627892dcd63a`;
the upstream failure-report SHA-256 is
`1b5a37884bf2c1e476890fae678be87286945eec043ed2d294067dee77a1d9ed`.

## Commands that exist today

The offline observer uses synthetic rows only and performs no external calls:

```sh
make public-bench-contract
```

It rejects empty and truncated corpora, incomplete address queries, missing
provenance, insufficient per-country or `unknown_lineage` counts, stale output
paths, endpoint redirects and cache/status-identity mismatches. It also reverses
the latitude/longitude mapping: the synthetic baseline must be 100% and the
mutant must fall close to zero. This proves observer sensitivity, not product
accuracy.

Generate and validate the fixed offline hybrid schema-v4 truth with the pinned
recipe before running the benchmark. `make public-bench` never invokes the
legacy Wikidata fetch helper and never downloads or replaces truth.

The manual runner requires that truth and its `.manifest.json` sidecar, an
executable GridPin binary, four operator-supplied **local release-candidate
sheets**, a fresh non-existing result path, and at least one selected
competitor. A Photon-only run is:

```sh
make public-bench \
  PUBLIC_BENCH_CONTACT='quality@example.com' \
  TRUTH=public-bench-work/hybrid-truth-corpus-v4.jsonl \
  RESULT=public-bench-work/results-photon-20260801T120000Z.json \
  GRIDPIN_BIN=/absolute/path/gridpin \
  SHEET_FR=/path/france.bin SHEET_IT=/path/italy.bin \
  SHEET_NL=/path/netherlands.bin SHEET_RS=/path/serbia.bin \
  COMPETITOR=photon \
  PHOTON_URL=http://127.0.0.1:2323 \
  PHOTON_STATUS_EVIDENCE=public-bench-work/photon-status-20260801T115900Z.json \
  NOMINATIM_NOT_RUN_REASON='not selected for this single-service run'
```

For a one-time, policy-compliant public Nominatim run, keep the common truth,
binary and four sheet arguments above and replace the service arguments with:

```sh
COMPETITOR=nominatim \
NOMINATIM_URL=https://nominatim.openstreetmap.org \
NOMINATIM_STATUS_EVIDENCE=public-bench-work/nominatim-status-20260801T115900Z.json \
PHOTON_NOT_RUN_REASON='not selected for this single-service run' \
ALLOW_PUBLIC_NOMINATIM=1 \
PUBLIC_BENCH_PAUSE=1.1
```

Use
`COMPETITOR='photon nominatim'` and supply both endpoint/evidence pairs to run
both. Every unselected service requires an operator-supplied `*_NOT_RUN_REASON`;
it receives an explicit `not_run` record and no score.

Status evidence is a retained schema-1 JSON wrapper around the selected
service's status response. Photon evidence binds the derived `/status` endpoint
and must contain `status: "Ok"`, `version`, `git_commit` and `import_date`.
Nominatim evidence binds `/status?format=json` and must contain `status: 0`,
`software_version`, `database_version` and `data_updated` or
`data_updated_at`. Both wrappers contain `schema`, `engine`, `endpoint`,
`fetched_at` and the raw object under `response`. Capture it immediately before
the run, preserve the exact query endpoint pairing, and keep it with the result.
Its SHA-256 participates in endpoint identity, cache identity and the score.

Capture the wrapper with the same runner immediately before a manual run; do
not hand-compose it or use shell redirection:

```sh
python3 examples/public_benchmark.py capture-status \
  --engine photon --url http://127.0.0.1:2323 \
  --output public-bench-work/photon-status-20260801T115900Z.json \
  --user-agent 'GridPin-public-benchmark/1.0 (quality@example.com)'

python3 examples/public_benchmark.py capture-status \
  --engine nominatim --url https://nominatim.openstreetmap.org \
  --output public-bench-work/nominatim-status-20260801T115900Z.json \
  --user-agent 'GridPin-public-benchmark/1.0 (quality@example.com)'
```

`capture-status` derives the exact status endpoint from the supplied query
origin, refuses redirects, validates the engine-specific response, retains the
raw response and UTC timestamp, and atomically refuses an existing output or
symlink. Keep the resulting file and its SHA-256 with the result.

The canonical public Photon demo is forbidden even with an allow flag: use a
self-hosted Photon instance. One instance must answer all four countries,
because the runner sends every row to a single `PHOTON_URL` and distinguishes
countries only by the `countrycode` parameter. The per-country databases
published by GraphHopper cannot be combined into one data directory, and a
Nominatim import is not required. Stream the published planet dump through
Photon's own importer and let it filter, which yields one database with one
import date and one status identity:

```sh
mkfifo planet.fifo
curl -sSL https://download1.graphhopper.com/public/photon-dump-planet-1.0-latest.jsonl.zst \
  | zstd -dc > planet.fifo &
java -jar photon.jar import -data-dir . -import-file planet.fifo \
  -country-codes fr,it,nl,rs -languages en,fr,it,nl,sr -j 4
java -jar photon.jar serve -data-dir . -listen-port 2323
```

The dump is never stored: on the retained run the compressed dump was 24.3 GB
while the resulting four-country database was 12.4 GiB, and the two together did
not fit the available disk. Set `-languages` to cover the query language of
every measured country; the importer's default set (`en,de,fr,it`) omits Dutch
and Serbian and would understate Photon by configuration. The import processed
the whole planet stream in about 3.5 hours and produced 42,249,775 documents.
Stop the server as soon as the run finishes: it memory-maps the whole database
and will otherwise sit on several GB of RAM.

Self-hosted Nominatim is preferred. A deliberate
one-time run against the public Nominatim service additionally requires
`ALLOW_PUBLIC_NOMINATIM=1` and `PUBLIC_BENCH_PAUSE=1.1` or greater. The runner
is sequential, sends an identifying contact User-Agent and retains response
caches; the operator remains responsible for the service's current usage policy.

The runner validates sheet country/layer/source-release/license metadata, binds
all requests to the external truth and status evidence, rejects redirects, and
records hashes for the runner, binary, sheets, truth manifest and raw-response
caches. `RESULT` must not exist. A failed or partial run leaves that fresh result
absent; a partial response cache may remain for diagnosis, while a complete
cache is reusable only under the same corpus, endpoint, service version and
status-evidence hash.

## Post-release clean-clone asset route

The following command is the exact route from release `v0.1.0` onward. The two channels are **not**
interchangeable: the CLI archive and `SHA256SUMS` come from the GitHub release,
while the country sheets are served from `dl.gridpin.dev` — GitHub carries no
sheet at all. It must fail until those assets exist; no clean-clone transcript is
claimed before then. The shell block supports Apple Silicon macOS and x86-64
Linux:

```sh
set -eu
git clone --branch v0.1.0 --depth 1 https://github.com/gridpin/gridpin.git gridpin-v0.1.0-bench
cd gridpin-v0.1.0-bench
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) cli_asset=gridpin-aarch64-apple-darwin.tar.gz ;;
  Linux-x86_64) cli_asset=gridpin-x86_64-unknown-linux-gnu.tar.gz ;;
  *) echo "no v0.1.0 CLI command documented for this platform" >&2; exit 2 ;;
esac
release_url=https://github.com/gridpin/gridpin/releases/download/v0.1.0
sheets_url=https://dl.gridpin.dev/v0.1.0
asset_dir=public-bench-work/release-v0.1.0
mkdir -p "$asset_dir"
cd "$asset_dir"
for asset in "$cli_asset" SHA256SUMS gridpin-release-signers; do   # GitHub release half
  curl --fail --location --remote-name "$release_url/$asset"
done
# The trust root comes from the OTHER half: an attacker who owns the GitHub release could swap
# the archive AND its SHA256SUMS line together, and a checksum check alone would still pass.
for proof in attestation.json attestation.json.sig; do
  curl --fail --location --remote-name "$sheets_url/$proof"
done
ssh-keygen -lf gridpin-release-signers    # compare by eye with the fingerprint in the README
ssh-keygen -Y verify -f gridpin-release-signers -I gridpin-release \
           -n gridpin-g02 -s attestation.json.sig < attestation.json \
  || { echo "signature does not verify — STOP" >&2; exit 1; }
# The archive you are about to unpack must be the one the owner signed for.
want=$(python3 -c "import json,sys;print([a['sha256'] for a in json.load(open('attestation.json'))['assets'] if a['name']==sys.argv[1]][0])" "$cli_asset")
test "$want" = "$(shasum -a 256 "$cli_asset" | cut -d' ' -f1)" \
  || { echo "CLI archive does not match the signed attestation — STOP" >&2; exit 1; }
for sheet in france.bin italy.bin netherlands.bin serbia.bin; do   # sheets: dl.gridpin.dev only
  curl --fail --location --remote-name "$sheets_url/$sheet"
done
awk -v cli="$cli_asset" '
  $2 == cli || $2 == "france.bin" || $2 == "italy.bin" ||
  $2 == "netherlands.bin" || $2 == "serbia.bin" { print; seen[$2] = 1 }
  END {
    if (!(cli in seen) || !("france.bin" in seen) || !("italy.bin" in seen) ||
        !("netherlands.bin" in seen) || !("serbia.bin" in seen)) exit 2
  }
' SHA256SUMS > SHA256SUMS.selected
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum --check SHA256SUMS.selected
else
  shasum -a 256 --check SHA256SUMS.selected
fi
tar -xzf "$cli_asset"
cd ../..
```

Continue with the Overture truth recipe and status capture, then use the manual
command above with `GRIDPIN_BIN="$PWD/public-bench-work/release-v0.1.0/gridpin"`
and sheet paths under the same release directory. This route uses released
artifacts instead of hidden build inputs; it does not remove the truth-lineage
disclosure and selected-service evidence requirements.

## License review

No third-party benchmark corpus is embedded in this repository.

| Component or dataset | License / policy | Embed in Git? | Runtime use? | Decision |
|---|---|---:|---:|---|
| [geocoder-tester](https://github.com/geocoders/geocoder-tester) code | MIT | Yes, but not needed | Yes | Not vendored. Commit `5384d1534bc3c59e8d280be3d951a92356ce470b` was run directly: 1/1 loose forward smoke passed. The complete 285-case Alsace numbered file improved from the historical 0/285 result to 173/285 after forward hits exposed the actually matched house number, to 208/285 with adapter-only focus, and to 230/285 with the conservative engine-plus-adapter focus. |
| Unmarked geocoder-tester test files | Public domain according to its README | Legally possible | Yes | Not copied: documented subsets do not supply the required four-country address scale or lineage proof. |
| geocoder-tester `world/airports.tsv` / OpenFlights | ODbL-1.0 | No | Not selected | Rejected: airport/station data is not a general address truth corpus and the ODbL dataset is not embedded. |
| geocoder-tester Wikidata-derived files | CC0-1.0 according to its README | Possible | Yes | Not copied: row-level statement references and query completeness are not established by the package description. |
| [Overture Places](https://docs.overturemaps.org/guides/places/) release `2026-06-17.0` | Mixed per-source terms retained from every `SourceItem`; no blanket license is inferred | Corpus stays outside Git | 200-row component of the fixed hybrid truth | The extractor retains row/source licenses, labels missing terms `UNKNOWN`, and fails closed for strong lineage classes. Provider redistribution terms must be reviewed before any corpus publication. |
| [Geofabrik OpenStreetMap extracts](https://download.geofabrik.de/) at `2026-07-01T20:22:00Z` | ODbL-1.0; OpenStreetMap contributors | Corpus stays outside Git | Fixed hybrid truth input | Exact Alsace, Isole, Drenthe, and Serbia PBF byte counts, replication sequences, URLs, and SHA-256 values are pinned. All selected OSM rows remain `unknown_lineage`; 1,000 such rows make Nominatim source-related and the score descriptive only. |
| [Wikidata structured data](https://www.wikidata.org/wiki/Wikidata:Licensing) | CC0-1.0 | Corpus stays outside Git | Optional legacy helper | Not used by `make public-bench` and not the primary recipe. A Wikidata publication layer alone does not prove factual lineage or sufficient four-country volume. |
| [Photon](https://github.com/komoot/photon) software and service data | Apache-2.0 code; OSM-derived data under ODbL | No service database or responses | Selected self-hosted query only | Runtime comparator. Public `photon.komoot.io` is rejected; status evidence, raw responses and attribution stay in the external run directory. |
| [Nominatim](https://github.com/osm-search/Nominatim) software and service data | GPL code; OSM-derived data under ODbL | No service database or responses | Selected query only | Self-hosting is preferred. A one-time public-service run requires the explicit allow flag, policy-compliant rate/contact and retained cache. |
| GridPin country sheets | Per-country data license in sheet manifest | Not in benchmark | Operator-supplied local release-candidate artifact | Required at runtime and country/layer/release/license/hash checked. The exact `v0.1.0` route is available across the published GitHub and `dl.gridpin.dev` release channels. |

## What is still unverified

- national representativeness for France, Italy, and the Netherlands: their OSM
  inputs are Alsace, Isole, and Drenthe regional extracts;
- independent factual lineage for `unknown_lineage` rows. In particular, OSM
  truth is source-related to both Nominatim and Photon, and an `unknown`
  relationship is not a proof of independence;
- a general independence claim. On the strictly independent 230-row corpus
  GridPin leads both competitors, significantly against Photon and not
  significantly against Nominatim (p = 0.0652). That corpus rests on one
  upstream provider and contains only POI rows, so it does not license a
  product-wide statement about address geocoding;
- complete numbered-address and in-country reverse geocoder-tester quality.
  The reverse Photon contract is implemented and the upstream world file is
  consumed without protocol errors, but that file contains no French case and
  therefore cannot measure the one-country France sheet. The
  Alsace numbered file now scores **230/285**, up from 173/285 after matched
  house numbers were exposed and from the 208/285 adapter-only focus baseline.
  The engine can now recover exact same-name streets near the point even when
  the global prefix cap omitted them; the adapter then orders the widened pool.
  The remaining 55 mismatches are still open quality work. The benchmark does
  not establish national representativeness, and its component comparison does
  not turn into a coordinate-accuracy claim;
- publication and independent checksum verification of the `v0.1.0` binary,
  four sheets and `SHA256SUMS` from a clean public clone. The owner moved this to
  a post-release milestone rather than treating it as a pre-release failure;
- an executed and reviewed 1,000-or-more-row private corpus. The recipe records
  candidate sources and capacity, but no such corpus/hash is claimed;
- immutability of the public Nominatim database during the 31-minute request
  window. Recalculation from the sealed cache is reproducible; a new live run is
  a new observation.

All three services are now populated from one retained result: the 2026-08-04
three-service run above. The rule that produced the earlier `not_run` still
stands — a service without its own measured run is reported as a status, never
as a numeric zero — and estimates, partial-country runs and values from another
corpus remain forbidden substitutes.
