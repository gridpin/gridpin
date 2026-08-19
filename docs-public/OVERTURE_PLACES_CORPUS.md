# Public truth-corpus recipe

This recipe documents both the retained Overture acquisition and the fixed
offline hybrid assembly that produces the public benchmark for France,
Italy, the Netherlands, and Serbia. It also records the evidence and design
work needed for a future private expansion from 300 to 1,000 or more rows per
country.

The input is the pinned Overture release `2026-06-17.0`, public S3 path:

```text
s3://overturemaps-us-west-2/release/2026-06-17.0/theme=places/type=place/*.parquet
```

The distinction between themes is essential. This recipe reads
`theme=places`, **not** `theme=addresses`. Italy, the Netherlands, and Serbia
GridPin address sheets are built from Overture's address theme, and using that
same export as truth would be circular. A Places record is a separate export,
but it can still share an upstream provider with a GridPin sheet; every row
therefore carries a disclosed lineage class instead of claiming blanket
independence.

Official references:

- [Overture data access](https://docs.overturemaps.org/getting-data/)
- [Places theme guide](https://docs.overturemaps.org/guides/places/)
- [Overture attribution and per-source licenses](https://docs.overturemaps.org/attribution/)
- [CDLA Permissive 2.0](https://cdla.dev/permissive-2-0/)
- [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
- [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/)
- [Open Database License 1.0](https://opendatacommons.org/licenses/odbl/1-0/)

## What the extractor guarantees

[`examples/overture_places_corpus.py`](../examples/overture_places_corpus.py)
uses DuckDB 1.5.3 and Python's standard library around it. The executable
contract pins CPython 3.11.5 and Unicode data 14.0.0. It:

- reads the release and theme pinned above;
- pins `type=place` instead of discovering a wildcard partition;
- couples each country code to its own bounding box in the scan predicate, so
  GeoParquet row-group pruning can avoid a global materialization;
- issues one combined S3 query for all requested countries and never performs a
  global `COUNT`; the scan keeps a bounded `min_by` heap per country and coarse
  source cohort, further split into 256 stable diversity shards, instead of
  letting dominant Foursquare rows or a few cities/categories displace rarer
  candidates;
- relies on the pinned Place schema's Point geometry contract, verifies that
  each retained candidate has equal bbox minima/maxima, and reads the identical
  point coordinate from `bbox.xmin`/`bbox.ymin`; this avoids installing the
  spatial extension and decoding WKB in the hot scan;
- carries complete SourceItem structs through the bounded heaps and calls
  DuckDB `to_json` only after a candidate was retained, preserving DuckDB's
  lossless nested/date serialization without serializing every eligible row;
- retains approximately `per-country * candidate-multiplier` candidates per
  country and coarse source cohort (`outside_hint`, `common_hint`,
  `unknown_hint`), distributed across 256 shards made from locality, category,
  network, and coarse coordinates; those cohorts and shards affect acquisition
  only—authoritative lineage is evaluated from complete source metadata in
  Python;
- writes the complete raw candidate snapshot and its acquisition manifest
  before validation or final selection, so a failed quota can be diagnosed and
  reprocessed offline without another S3 read;
- uses two explicit deterministic hash stages, never Parquet order or `first
  N`; both formulas are recorded in the manifest rather than described as one
  interchangeable hash;
- rejects non-finite or out-of-product-bounds rows; street addresses must have
  alphabetic text and a number, municipalities must have alphabetic text, and
  every non-empty postcode must contain a digit;
- deduplicates Overture record id, normalized full query, and coordinates
  rounded to five decimal places;
- applies caps by municipality, category, and named network;
- requires 300 rows per country and at least 150 `unknown_lineage` rows per
  country by default;
- emits the generic public-benchmark schema v3 plus a normalized subset of the
  selected Overture address and all fields of every retained Overture
  `SourceItem` for later inspection;
- writes selection diagnostics even on a fail-closed result, and an adjacent
  manifest on success with the corpus SHA-256, row counts, lineage
  matrix, retrieval timestamp, release, exact command, script hash, DuckDB
  version, selection seed, both selection formulas, and diversity caps.

The two selection stages deliberately serve different purposes:

1. DuckDB scans the exact `type=place` partition once, keeps one bounded
   `min_by` heap per country on SHA-256 of
   `COALESCE(CAST(id AS VARCHAR),'')|COALESCE(CAST(addresses[1].freeform AS VARCHAR),'')|COALESCE(CAST(bbox.ymin AS VARCHAR),'')|seed`,
   breaks ties by record id, and applies the candidate limit independently to
   each country, coarse source cohort, and stable diversity shard. The
   per-shard limit is the ceiling of the requested cohort pool divided by 256.
   SQL cohorts and shards are sampling hints, not published lineage claims.
2. After Python validates and normalizes the complete rows, final selection
   sorts on SHA-256 of
   `seed|release|country|record_id|NFKC-casefold-alnum-space(query)|latitude:.7f|longitude:.7f`,
   again breaking ties by record id, before lineage quota, deduplication, and
   diversity caps are applied.

These formulas are not claimed to be the same. The pinned DuckDB version,
release, seed, command, formulas, and resulting corpus hash together form the
reproduction record.

The per-country candidate limit bounds heap memory and the result transferred
from DuckDB to Python; it does not promise a proportional reduction in S3
bytes. Finding the globally smallest deterministic hashes still requires
visiting every applicable row group and eligible row. Consequently, a
20-row diagnostic can still take time. The optimized query removes avoidable
work—four repeated scans, wildcard partition discovery, WKB decoding, and
pre-selection SourceItem JSON serialization—without pretending that a stable
global rank can short-circuit after the first 20 storage-order rows.

The original global-pool design had two fail-closed attempts on 2026-08-01.
The console-only 12× attempt reported 126 diverse `unknown_lineage` rows for
France against the required 150, but no complete transcript was retained. The
24× attempt reported the same 126 and has a reconstructed, explicitly labelled
terminal record; neither run produced a corpus, manifest, or raw pool. The
number 126 is therefore a result of the old greedy selection over its retained
pool, not proof that Overture contains only 126 eligible rows.

After explicit owner approval, the next acquisition changes the architecture
rather than repeating the multiplier loop: it stratifies the one bounded scan
by source cohort and preserves the raw snapshot before selection. It is still
honestly a third full S3 read. Quotas (300/150), release, seed, deduplication,
and diversity caps are not relaxed. Once that snapshot exists, every later
selection attempt must use `--from-acquisition` and perform zero network reads.

The DuckDB process is capped at 2 GiB memory and 2 GiB temporary disk. Before
extension installation and before the combined S3 query, the script checks
free disk. Below 5 GiB it stops. Below 10 GiB it warns that a second heavy
service must not be started. It never deletes a cache or another run's files.

## Conservative lineage policy

Coordinate lineage follows Overture's property-level source semantics. If one
or more source entries have `property=/geometry`, those entries are the
coordinate sources. Their disclosed `dataset` and `provider` identities are
both screened. Otherwise, the extractor falls back to the root source
entries whose `property` is empty. Other property sources, such as
`/properties/confidence`, are retained byte-for-JSON-value for inspection but do not
change coordinate lineage.

This distinction is fail-closed. A root Foursquare record with an
OpenStreetMap `/geometry` override is `unknown_lineage`: OSM does not prove the
exact national ancestor of any of the four indexed address sheets. A root
Foursquare record with an unfamiliar geometry provider is unknown for the same
reason. Multiple coordinate source records are also unknown unless exactly one
country-specific common ancestor can be proved from their disclosed names.

| Class | Rule |
|---|---|
| `outside_chain` | Exactly one effective coordinate source is disclosed; every available dataset/provider identity identifies Foursquare; its upstream `record_id` is a restricted ASCII token; the record gets a row-resolvable Foursquare review URL; and the SourceItem contains one exact, non-ambiguous declared license. |
| `common_upstream` | The effective source names exactly one ancestor recorded for that country's indexed sheet: FR = BAN; IT = ANNCSU or OpenAddresses; NL = BAG/Kadaster or OpenAddresses; RS = RGZ or OpenAddresses. Every effective coordinate SourceItem must declare an exact license. The canonical ancestor and its source page are stored in coordinate provenance. |
| `unknown_lineage` | OSM, AllThePlaces, Meta, Microsoft, Overture, an absent or unfamiliar source, multiple distinct common ancestors, an unsafe/missing Foursquare id, or any missing/ambiguous coordinate license. An unfamiliar source is never promoted to `outside_chain`. |

If an otherwise outside-chain source has no safe upstream record id, the row is
unknown. Its public provenance then names the Overture Places record and its
Overture UUID; the extractor never labels that UUID as a Foursquare record.
AllThePlaces remains unknown because the recipe has no documented
row-resolvable evidence link for it. Empty optional categories are omitted
rather than emitted as invalid blank metadata.

Licenses are never inferred from a provider name. The exact disclosed
SourceItem wording is preserved. A missing license is written as `UNKNOWN`; a
missing or visibly ambiguous license prevents promotion to `outside_chain` or
`common_upstream`. This avoids the earlier pseudo-license that combined a
blanket Overture term with unspecified provider terms.

This is disclosure, not proof that all upstream reuse has been discovered.
Provider names and the Overture attribution page must be reviewed again whenever
the release pin changes.

Each JSONL row includes these benchmark fields:

```json
{
  "country": "FR",
  "street_address": "10 Example Street",
  "postcode": "75001",
  "municipality": "Paris",
  "query": "10 Example Street, 75001 Paris, France",
  "lat": 48.0,
  "lon": 2.0,
  "record_id": "overture-place-id",
  "source_release": "2026-06-17.0",
  "source_url": "https://docs.overturemaps.org/guides/places/",
  "license": "Apache-2.0",
  "retrieved_at": "2026-08-01T00:00:00+00:00",
  "lineage_class": "outside_chain",
  "coordinate_provenance": {
    "source_name": "Foursquare",
    "source_url": "https://docs.overturemaps.org/guides/places/",
    "record_id": "direct-provider-record-id",
    "retrieved_at": "2026-08-01T00:00:00+00:00",
    "license": "Apache-2.0",
    "common_ancestor": null,
    "evidence_url": "https://foursquare.com/placemakers/review-place/direct-provider-record-id",
    "same_export_as_indexed_sheet": false
  }
}
```

Additional fields preserve category, confidence, brand/network, the structured
address, coordinate-source scope, coordinate and root source records, all
property-level `SourceItem` objects (including available `license`, `provider`,
`resource`, `version`, `update_time`, and source confidence fields), and the
row's stable selection hash.

Extractor policy `overture-places-root-source-v1` was invalid: it ignored
`/geometry` overrides and could label shared coordinates as outside-chain.
Policy `overture-places-coordinate-source-v2` was also invalid: it treated OSM
and OpenAddresses as globally shared ancestors, accepted AllThePlaces as
outside-chain without row-resolvable evidence, linked only to generic
attribution, and inferred licenses. Do not reuse a corpus or manifest made by
either policy. Version 3 identifies itself as
`overture-places-coordinate-source-v3` and uses a new selection seed.

## Exact 300-row-per-country command

Run from the public repository root. The generated environment and corpus live
under `public-bench-work/`, which is Git-ignored.

```sh
df -h .
python3.11 -m venv public-bench-work/overture-corpus-venv
public-bench-work/overture-corpus-venv/bin/python -m pip install 'duckdb==1.5.3'
df -h .
public-bench-work/overture-corpus-venv/bin/python \
  examples/overture_places_corpus.py \
  --acknowledge-network-read \
  --acknowledge-instrumented-acquisition \
  --per-country 300 \
  --min-unknown-per-country 150 \
  --candidate-multiplier 24 \
  --retrieved-at 2026-08-01T00:00:00Z \
  --acquisition-output public-bench-work/overture-places-2026-06-17.0.acquisition.jsonl \
  --output public-bench-work/overture-places-2026-06-17.0.jsonl
```

Choose the real UTC retrieval time before a production run. Supplying it
explicitly makes reruns byte-reproducible when the same pinned release and
selection options are used. The script also prints the final corpus SHA-256.
Verify it independently:

```sh
shasum -a 256 public-bench-work/overture-places-2026-06-17.0.jsonl
python3 examples/public_benchmark.py validate \
  public-bench-work/overture-places-2026-06-17.0.jsonl
```

The validator command may change with the benchmark CLI; the stable contract is
the schema-v3 JSONL plus its `.manifest.json` sidecar. Run
`python3 examples/public_benchmark.py --help` before the retained benchmark
transcript and record the exact accepted command.

If final selection fails but the acquisition manifest says `complete`, re-run
only the local stage with a fresh output name:

```sh
public-bench-work/overture-corpus-venv/bin/python \
  examples/overture_places_corpus.py \
  --from-acquisition public-bench-work/overture-places-2026-06-17.0.acquisition.jsonl \
  --per-country 300 \
  --min-unknown-per-country 150 \
  --candidate-multiplier 24 \
  --output public-bench-work/overture-places-2026-06-17.0-offline.jsonl
```

Offline mode rejects network acknowledgement flags and verifies the raw SHA,
row/count matrix, pinned query hash, release, countries, seed, candidate limit,
runtime identity, and relative artifact path before processing.

## Fixed offline hybrid corpus (schema v4)

[`examples/hybrid_truth_corpus.py`](../examples/hybrid_truth_corpus.py) builds
the reviewed 1,200-row hybrid truth set without any network access. It accepts
only the retained Overture acquisition above and these exact Geofabrik
snapshots: Alsace, Isole, Drenthe and Serbia at the `2026-07-01T20:22:00Z`
replication point. The script pins every filename, byte count, SHA-256,
replication base URL and sequence. It uses DuckDB's already-installed spatial
extension through `LOAD spatial`; it never executes `INSTALL` or downloads a
replacement input.

Run from the `code/` directory after placing the four reviewed PBF files under
`public-bench-work/sources/`:

```sh
.venv-py/bin/python examples/hybrid_truth_corpus.py \
  --overture-acquisition public-bench-work/overture-places-2026-06-17.0-instrumented-v3.acquisition.jsonl \
  --osm FR=public-bench-work/sources/alsace-260701.osm.pbf \
  --osm IT=public-bench-work/sources/isole-260701.osm.pbf \
  --osm NL=public-bench-work/sources/drenthe-260701.osm.pbf \
  --osm RS=public-bench-work/sources/serbia-260701.osm.pbf \
  --assembled-at 2026-08-01T23:05:27Z \
  --output public-bench-work/hybrid-truth-corpus-v4-final-20260801T230527Z.jsonl
```

The four PBF pins are:

| Country input | Bytes | SHA-256 | Replication sequence |
|---|---:|---|---:|
| Alsace | 129,643,154 | `f8a63f9a31864821a16fa1fd1fd2626a587c4ea2d780a2d863bfa361d19bfaa7` | 4830 |
| Isole | 212,733,976 | `b820ee216ef76b326bf1306ea946abfbfe58bdf08db9147289cf556d22665e88` | 3893 |
| Drenthe | 62,699,767 | `2814290012b08420820e2ef47373156707128de68a2b15783302e8a6feafa326` | 2774 |
| Serbia | 236,966,213 | `0d5e526a7411e6a0dd7400bf188392d79de17477bd0612dee216a5a255fb83d0` | 4835 |

All four report the PBF timestamp `2026-07-01T20:22:00Z`; the exact URLs and
replication-base metadata are constants in the assembler and are repeated in
the resulting source catalog.

For each country the fixed selection is 50 Overture Places rows and 250 direct
OSM address nodes. Every OSM row remains `unknown_lineage`; it is never
promoted merely because OSM is a different export. Before selection the script
excludes every member of any cross-source normalized-query or rounded-coordinate
collision. It then applies municipality 12, physical street 2, 0.01-degree
cell 3, non-empty category 24 and non-empty network 9 caps. No geocoder output
is read or ranked.

The adjacent manifest carries the complete country/source/lineage matrix,
requested and realized source quotas, source catalog and retained artifact
hashes. Each row repeats its source id, family, snapshot, license, artifact
SHA-256 and coordinate provenance. A diagnostics sidecar is written on both
successful selection and a fail-closed quota/cap result. The output, manifest
and diagnostics are no-replace artifacts protected by a persistent file lock;
use a fresh output name for every attempted reproduction.

The retained output contains exactly 300 rows per country: 50 Overture Places
and 250 direct OSM rows. Its corpus SHA-256 is
`d62033a60c434fe1d9a8937681cb014e8d2f75bccb934df465a791dda227433f`;
the adjacent manifest SHA-256 is
`9e7fcd2c2579da51a56b66c9019c7b30f97d46caf9fc88661d8386957d442be4`.
The observed eligible OSM candidate pools were FR 29,646, IT 505,197, NL
277,672, and RS 10,564. Those counts demonstrate source capacity, not an
already validated 1,000-row benchmark. A future private 1,000-row hybrid must
be a separately reviewed recipe with new source quotas and scaled caps. Do not
alter these pins or silently relax caps and retain the same schema/hash
identity.

## Experimental Overture-only expansion to 1,000 or more rows

The convenience flag below requests 1,000 Overture rows per country while
keeping the 150-row minimum for the weakest lineage class. It is an acquisition
experiment, not the endorsed scaling route for the retained hybrid corpus:

```sh
df -h .
public-bench-work/overture-corpus-venv/bin/python \
  examples/overture_places_corpus.py \
  --acknowledge-network-read \
  --acknowledge-instrumented-acquisition \
  --future-per-country \
  --retrieved-at 2026-08-01T00:00:00Z \
  --acquisition-output public-bench-work/overture-places-private-1000.acquisition.jsonl \
  --output public-bench-work/overture-places-private-1000.jsonl
```

For a larger experimental target, pass a value, for example
`--future-per-country 2500`. Automatic diversity caps scale with the target.
If a country cannot fill the requested target, the extractor fails rather than
relaxing a cap silently. A reviewer may raise an explicit `--max-per-city`,
`--max-per-category`, or `--max-per-network` only after inspecting the rejected
distribution. Increasing `--candidate-multiplier` expands the deterministic
provider pools without changing the ordering rule.

The retained 300-row work showed that the pinned Overture snapshot alone could
not fill the required four-country mix under the reviewed caps: France, the
Netherlands, and Serbia were below 300 usable rows in the acquired candidate
set. Therefore a successful `--future-per-country 1000` result is not claimed.
The practical future route is to scale the direct OSM component from the fixed
PBF inputs (or a newly pinned, independently reviewed snapshot), define new
Overture/OSM quotas and caps, rerun collision exclusion, and publish a new
schema/hash identity. Keep any expanded JSONL and its manifest private unless
every selected
provider's redistribution terms have been reviewed. The Places theme is a
mixture, not one blanket CDLA dataset; the recipe preserves declared license
wording and writes `UNKNOWN` where none is disclosed. The safe Git artifact is
this recipe, not the downloaded corpus.

## Larger direct-source candidate retained for a later private corpus

[Foursquare Open Source Places](https://docs.foursquare.com/data-products/docs/access-fsq-os-places)
is the strongest larger candidate found during this work. Its
[schema](https://docs.foursquare.com/data-products/docs/places-os-data-schema)
includes point geometry, structured address fields, categories, confidence and
source identity, and its
[release notes](https://docs.foursquare.com/data-products/docs/fsq-os-places-release-notes)
describe a global collection large enough for a 1,000-or-more-row target in
these countries. Direct access currently requires a free portal token, so this
repository does not embed a signed URL, credential or downloaded release. Terms
must be checked against the current
[open-source notice](https://opensource.foursquare.com/places-notice-txt/) at
retrieval time.

The future repacking recipe must preserve the multi-source disclosure model; it
must not be relabelled as the current fixed schema v4 without a new reviewed
source catalog and validator contract:

1. obtain the current GeoParquet release URL through the documented portal and
   keep the token outside commands, manifests and Git;
2. record the release id, retrieval UTC time, source URL, notice URL and file
   SHA-256 before filtering;
3. retain only FR/IT/NL/RS rows with Point geometry, a numbered street address,
   an alphabetic municipality, and a numeric-bearing postcode when one is
   present;
4. map `fsq_place_id` to `record_id`, the address components to
   `street_address`/`postcode`/`municipality`, and geometry latitude/longitude;
   also assign a new pinned `truth_source_id`, `truth_source_family`, snapshot,
   artifact hash, license, and source-catalog entry on every row;
5. set coordinate provenance to the row-resolvable Foursquare record, preserve
   the exact release license, and keep
   `same_export_as_indexed_sheet=false`; do not promote a row to
   `outside_chain` if its id, license or coordinate-source evidence is absent;
6. apply the same normalized-query/coordinate deduplication, cross-source
   collision exclusion, stable SHA-256 ordering, city/category/network caps and
   country/source/lineage manifest matrices used by the hybrid recipe, with
   1,000 rows per country as the initial target;
7. add and review a new schema/catalog contract in `public_benchmark.py` before
   validation; the current schema-v4 validator intentionally accepts only the
   exact Overture-plus-four-Geofabrik source catalog documented above. Keep the
   corpus private until the current redistribution terms have been reviewed.

The implemented Overture extractor already reaches Foursquare-sourced Places
rows without requiring a portal token and exposes an unverified
`--future-per-country 1000` target mode. No 1,000+ corpus has yet been executed
or hashed, so successful production at that size is not claimed. Direct FSQ
access is retained as a future option because it removes an aggregation layer
and exposes a much larger provider-native pool; it is not represented as an
executed or hashed corpus in this report.

## Accuracy and representativeness limits

Overture Places is large and geographically diverse, but it is a POI collection,
not an unbiased address register. Commercial venues, chains, and urban centres
can be overrepresented. Coordinates can denote a venue centroid or entrance
rather than a cadastral parcel. The city/category/network caps reduce those
biases but do not remove them. Report results by country and lineage class, keep
the 300 m hit threshold visible, and treat an empty geocoder response as a miss.
OSM-sourced truth also has comparator bias for Photon/Nominatim even though it
is not a proved common ancestor of the indexed national address sheet; keep
those rows in `unknown_lineage` and disclose that limitation in benchmark
interpretation.

Changing the release, seed, requested size, caps, or retrieval timestamp creates
a different corpus and therefore must create a new manifest and SHA-256. Never
reuse a previous hash under new metadata.
