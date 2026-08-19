# gridpin build targets. One country per command: make fr / make nl / make it / make rs
# Tiny end-to-end example + public smoke test: make mc && make smoke
BIN=gridpin/target/release/gridpin
# Private lab targets (quality bench, training, scrapes) live in Makefile.lab,
# which is not part of the public repository. -include keeps the monorepo working.
-include Makefile.lab
RULES=$(if $(wildcard rules),--rules rules,)
MODELS=--model ml/parser_v0.bin --rank ml/rank_v0.bin $(RULES)
GEONAMES_RS=data/geonames_rs.txt

# All recipes are phony (not files). Without this, a file named like a target (e.g. `regen-manifests`)
# in the working dir makes `make` consider the target up to date and silently skip the recipe — which
# let the release gate skip its provenance check.
.PHONY: engine bindings fr nl it rs fr-poi mc smoke test test-py test-duckdb \
	provision regen-manifests public-gate http public-bench-contract public-bench

engine:
	cargo build --release --manifest-path gridpin/Cargo.toml
# Everything runnable without private data: Rust unit tests, worker tests, Python
# bindings and DuckDB extension (against the Monaco smoke sheet). The full quality
# full quality gate needs private sheets and lives in Makefile.lab.
test: engine
	cargo test --release --manifest-path gridpin/Cargo.toml
	cargo test --release --manifest-path duckdb-ext/Cargo.toml
	$(MAKE) test-py test-duckdb

# Python bindings against the Monaco smoke sheet (build it first: make mc)
test-py: bindings mc
	.venv-py/bin/python -m pytest gridpin/tests/ -q

# DuckDB extension against the Monaco smoke sheet
test-duckdb: bindings mc
	GRIDPIN_TEST_SHEET=$$PWD/data/mc.bin .venv-py/bin/python -m pytest duckdb-ext/test/test_extension.py -q

# Rebuild ALL bindings after any engine change, or the Python wheel and the DuckDB
# extension keep the old engine. Prereqs: a Python venv with maturin at .venv-py/, and
# duckdb/extension-ci-tools cloned into duckdb-ext/extension-ci-tools (CI does both).
# Platform-aware: the library name and DuckDB platform differ per OS
# (a hardcoded .dylib/osx_arm64 made this target unrunnable on the Linux CI).
UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
  EXT_LIB := libgridpin_ext.dylib
  DUCKDB_PLATFORM := osx_arm64
else ifeq ($(UNAME_S),Linux)
  EXT_LIB := libgridpin_ext.so
  DUCKDB_PLATFORM := linux_amd64
else
  EXT_LIB := gridpin_ext.dll
  DUCKDB_PLATFORM := windows_amd64
endif
# Extension version comes from its Cargo.toml — the single source;
# -dv v1.2.0 is the C_STRUCT ABI floor (stable since DuckDB 1.2.0), not the dep version.
EXT_VERSION := $(shell grep -m1 '^version' duckdb-ext/Cargo.toml | sed 's/.*"\(.*\)"/\1/')
bindings: engine
	cd gridpin && VIRTUAL_ENV=$$PWD/../.venv-py ../.venv-py/bin/maturin develop --release
	cd duckdb-ext && cargo build --release && python3 extension-ci-tools/scripts/append_extension_metadata.py -l target/release/$(EXT_LIB) -n gridpin_ext -o gridpin_ext.duckdb_extension -p $(DUCKDB_PLATFORM) -dv v1.2.0 -ev v$(EXT_VERSION) --abi-type C_STRUCT

fr: engine
	nice -n 19 python3 prep/normalize.py
	nice -n 19 python3 prep/export_build.py
	GRIDPIN_REQUIRE_META=1 nice -n 19 $(BIN) build data/build_input.csv.gz data/france.bin $(MODELS) --meta data/fr_manifest.json

nl: engine
	nice -n 19 python3 prep/overture.py NL
	nice -n 19 python3 prep/export_build.py data/nl_norm.parquet data/build_nl.csv.gz
	GRIDPIN_REQUIRE_META=1 nice -n 19 $(BIN) build data/build_nl.csv.gz data/nl.bin $(MODELS) --meta data/nl_manifest.json

it: engine
	nice -n 19 python3 prep/overture.py IT
	nice -n 19 python3 prep/export_build.py data/it_norm.parquet data/build_it.csv.gz
	GRIDPIN_REQUIRE_META=1 nice -n 19 $(BIN) build data/build_it.csv.gz data/it.bin $(MODELS) --meta data/it_manifest.json

rs: engine
	test -f $(GEONAMES_RS) || python3 prep/fetch_zip.py https://download.geonames.org/export/dump/RS.zip RS.txt $(GEONAMES_RS)
	nice -n 19 python3 prep/overture.py RS $(GEONAMES_RS)
	nice -n 19 python3 prep/export_build.py data/rs_norm.parquet data/build_rs.csv.gz
	GRIDPIN_REQUIRE_META=1 nice -n 19 $(BIN) build data/build_rs.csv.gz data/rs.bin $(MODELS) --meta data/rs_manifest.json

# POI layer (opt-in, a SEPARATE file — never mixed into the address index).
# The engine cascades when you pass it: --poi <file> / Geocoder(poi=...) / gridpin_load_poi.
fr-poi: engine
	nice -n 19 python3 prep/overture_places_layer.py FR
	nice -n 19 python3 prep/export_build.py data/fr_poi_norm.parquet data/build_frpoi.csv.gz
	GRIDPIN_REQUIRE_META=1 nice -n 19 $(BIN) build data/build_frpoi.csv.gz data/fr_poi.bin $(MODELS) --meta data/frpoi_manifest.json

# Public smoke: a tiny country built from scratch — pipeline + engine on built-in defaults
mc: engine
	mkdir -p data
	# The smoke input is pinned to a versioned fixture, not a floating geofabrik `-latest`.
	# A `-latest` download let the SAME commit build from DIFFERENT bytes on different days
	# (geofabrik regenerates monaco daily), so a green smoke could not be reproduced. The fixture
	# is the retained europe/monaco extract; the checksum fails the build closed if the committed
	# bytes are ever altered or a partial checkout truncates them.
	cp eval/smoke/fixtures/monaco.osm.pbf data/monaco-latest.osm.pbf
	echo "58c25f4f88ae1321bf4fe45b0baf2c67cd1c4fe0441f8fde794c1074db46819a  data/monaco-latest.osm.pbf" | shasum -a 256 -c -
	nice -n 19 python3 prep/osm.py MC data/monaco-latest.osm.pbf
	nice -n 19 python3 prep/export_build.py data/mc_norm.parquet data/build_mc.csv.gz
	GRIDPIN_REQUIRE_META=1 nice -n 19 $(BIN) build data/build_mc.csv.gz data/mc.bin --meta data/mc_manifest.json

smoke: mc
	python3 eval/smoke/run.py $(BIN) data/mc.bin

# Photon-compatible loopback facade for interoperability tools such as
# geocoder-tester. One process serves one country sheet and has no dependencies
# beyond Python's standard library and the GridPin CLI.
http: engine
	test -n "$(INDEX)" || (echo "INDEX=/absolute/path/to/country.bin is required" >&2; exit 2)
	test -n "$(COUNTRY)" || (echo "COUNTRY=FR (or IT/NL/RS) is required" >&2; exit 2)
	python3 examples/gridpin_http.py --bin "$(BIN)" --index "$(INDEX)" --country "$(COUNTRY)"

# Offline observers for the public benchmark. This is safe for CI: it uses
# synthetic rows and a temporary fake CLI, never Wikidata or competitor APIs.
public-bench-contract:
	python3 eval/smoke/test_http_adapter_contract.py
	python3 eval/smoke/test_public_benchmark_contract.py
	python3 eval/smoke/test_public_benchmark_leaks.py

# Manual release comparison. It is intentionally excluded from CI. Truth,
# binary, sheets, endpoint status captures and the fresh result path are all
# externally supplied artifacts; this target never downloads or overwrites them.
PUBLIC_BENCH_CONTACT ?=
TRUTH ?=
RESULT ?=
GRIDPIN_BIN ?= $(BIN)
SHEET_FR ?=
SHEET_IT ?=
SHEET_NL ?=
SHEET_RS ?=
COMPETITOR ?=
PHOTON_URL ?=
PHOTON_STATUS_EVIDENCE ?=
PHOTON_NOT_RUN_REASON ?=
NOMINATIM_URL ?=
NOMINATIM_STATUS_EVIDENCE ?=
NOMINATIM_NOT_RUN_REASON ?=
ALLOW_PUBLIC_NOMINATIM ?=
PUBLIC_BENCH_PAUSE ?= 1.1
PUBLIC_BENCH_TIMEOUT ?= 30
PUBLIC_BENCH_WORK ?= public-bench-work/cache
PUBLIC_BENCH_SERVICE_ARGS = \
	$(if $(filter photon,$(COMPETITOR)),--competitor photon --photon-url "$(PHOTON_URL)" --photon-status-evidence "$(PHOTON_STATUS_EVIDENCE)") \
	$(if $(filter nominatim,$(COMPETITOR)),--competitor nominatim --nominatim-url "$(NOMINATIM_URL)" --nominatim-status-evidence "$(NOMINATIM_STATUS_EVIDENCE)") \
	$(if $(strip $(PHOTON_NOT_RUN_REASON)),--photon-not-run-reason "$(PHOTON_NOT_RUN_REASON)") \
	$(if $(strip $(NOMINATIM_NOT_RUN_REASON)),--nominatim-not-run-reason "$(NOMINATIM_NOT_RUN_REASON)") \
	$(if $(filter 1,$(ALLOW_PUBLIC_NOMINATIM)),--allow-public-services)
public-bench: public-bench-contract
	test -n "$(PUBLIC_BENCH_CONTACT)" || (echo "PUBLIC_BENCH_CONTACT is required" >&2; exit 2)
	test -f "$(TRUTH)" && test -f "$(TRUTH).manifest.json" || (echo "TRUTH and its .manifest.json sidecar are required" >&2; exit 2)
	test -n "$(RESULT)" && test ! -e "$(RESULT)" || (echo "RESULT must be a fresh, non-existing path" >&2; exit 2)
	test -x "$(GRIDPIN_BIN)" || (echo "GRIDPIN_BIN must name an executable GridPin binary" >&2; exit 2)
	test -f "$(SHEET_FR)" && test -f "$(SHEET_IT)" && test -f "$(SHEET_NL)" && test -f "$(SHEET_RS)" || (echo "all four SHEET_CC files are required" >&2; exit 2)
	test -n "$(COMPETITOR)" && test -z "$(filter-out photon nominatim,$(COMPETITOR))" || (echo "COMPETITOR must select photon, nominatim, or both" >&2; exit 2)
	$(if $(filter photon,$(COMPETITOR)),test -n "$(PHOTON_URL)" && test -f "$(PHOTON_STATUS_EVIDENCE)" || (echo "selected Photon requires PHOTON_URL and PHOTON_STATUS_EVIDENCE" >&2; exit 2),:)
	$(if $(filter nominatim,$(COMPETITOR)),test -n "$(NOMINATIM_URL)" && test -f "$(NOMINATIM_STATUS_EVIDENCE)" || (echo "selected Nominatim requires NOMINATIM_URL and NOMINATIM_STATUS_EVIDENCE" >&2; exit 2),:)
	$(if $(filter photon,$(COMPETITOR)),:,$(if $(strip $(PHOTON_NOT_RUN_REASON)),:,(echo "unselected Photon requires PHOTON_NOT_RUN_REASON" >&2; exit 2)))
	$(if $(filter nominatim,$(COMPETITOR)),:,$(if $(strip $(NOMINATIM_NOT_RUN_REASON)),:,(echo "unselected Nominatim requires NOMINATIM_NOT_RUN_REASON" >&2; exit 2)))
	python3 examples/public_benchmark.py run --truth "$(TRUTH)" \
		--gridpin-bin "$(GRIDPIN_BIN)" --sheet "FR=$(SHEET_FR)" --sheet "IT=$(SHEET_IT)" \
		--sheet "NL=$(SHEET_NL)" --sheet "RS=$(SHEET_RS)" \
		$(PUBLIC_BENCH_SERVICE_ARGS) \
		--user-agent "GridPin-public-benchmark/1.0 ($(PUBLIC_BENCH_CONTACT))" \
		--pause "$(PUBLIC_BENCH_PAUSE)" --timeout "$(PUBLIC_BENCH_TIMEOUT)" \
		--work "$(PUBLIC_BENCH_WORK)" --output "$(RESULT)"
