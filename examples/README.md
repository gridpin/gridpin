# Examples

Small, runnable examples. The two product examples below need a country sheet — download one
from <https://gridpin.dev/docs.html#delivery> (no account, no key) and pass its path.

For the quickest trial, run `batch.py` against a downloaded `france.bin`.

- `batch.py` — geocode five French addresses with the Python binding: `python batch.py /path/to/france.bin`
- `orders.sql` — geocode a table of addresses with the DuckDB extension: `duckdb -unsigned < orders.sql` (edit the country file path first)
- `gridpin_http.py` — expose one sheet through a small Photon-compatible HTTP
  endpoint for coordinate-forward interoperability tests:
  `python gridpin_http.py --bin /path/to/gridpin --index /path/to/france.bin --country FR`.
  From a geocoder-tester checkout, pass the base origin without `/api`:
  `python -m pytest geocoder_tester/world/france/alsace/test_from_user_input.csv --api-url http://127.0.0.1:2322 --api-type photon --loose-compare`.
  Optional `lat`/`lon` are validated, forwarded to the engine as `query
  --near`, and used by the adapter to re-rank its bounded wide candidate pool.
  The engine can therefore inject a same-name local street hidden by the global
  prefix cap, while the adapter preserves the shipped distance ordering. Exact
  and near hits expose the stored house number, interpolation exposes the
  requested number, and the adapter maps it to Photon `housenumber`. The exact
  `/reverse` endpoint is also supported.
- `public_benchmark.py` — validate an externally supplied provenance-bearing
  schema-v3 or fixed hybrid schema-v4 corpus and
  compare GridPin with the explicitly selected Photon and/or Nominatim service
  using one metric. Selected services require endpoint-bound status evidence;
  the public Photon demo is forbidden, public Nominatim needs an explicit
  one-time allow flag, and every run needs a fresh result path. A retained
  four-country hybrid production run is described and interpreted in
  [`docs-public/BENCHMARK.md`](../docs-public/BENCHMARK.md).
- `overture_places_corpus.py` — extract a stable, provenance-bearing
  four-country candidate set from the pinned Overture Places release, with
  conservative lineage classes and an experimental, not-yet-proven 1,000+ row
  private-acquisition mode; see [`docs-public/OVERTURE_PLACES_CORPUS.md`](../docs-public/OVERTURE_PLACES_CORPUS.md).
- `hybrid_truth_corpus.py` — offline-only assembly of the fixed 1,200-row
  schema-v4 benchmark from the retained Overture acquisition plus pinned
  Geofabrik snapshots; it verifies every input hash and writes a no-replace
  corpus, manifest, and diagnostics sidecar. See the same corpus recipe above.
- `multiregion_truth_corpus.py` — low-memory schema-v5 extraction and assembly:
  twelve sequential one-region/PBF processes, bounded candidate heaps, and
  full-stream collision checks against the retained Overture universe. See
  [`docs-public/MULTIREGION_CORPUS.md`](../docs-public/MULTIREGION_CORPUS.md).
- `multiregion_benchmark.py` — validate schema-v5 manifest/region coupling and
  run local GridPin batches, reporting direct-OSM region spread and Overture
  countrywide scores separately. Results are always descriptive-only and
  headline-ineligible.
