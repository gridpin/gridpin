# gridpin build targets. One country per command: make fr / make nl / make it / make rs / make es
# Tiny end-to-end example + public smoke test: make mc && make smoke
BIN=gridpin/target/release/gridpin
# Private lab targets (quality bench, training, scrapes) live in Makefile.lab,
# which is not part of the public repository. -include keeps the monorepo working.
-include Makefile.lab
RULES=$(if $(wildcard rules),--rules rules,)
MODELS=--model ml/parser_v0.bin --rank ml/rank_v0.bin $(RULES)
GEONAMES_RS=data/geonames_rs.txt
GEONAMES_US=data/geonames_us.txt

# All recipes are phony (not files). Without this, a file named like a target (e.g. `smoke`)
# in the working dir makes `make` consider the target up to date and silently skip the recipe — which
# let the release check skip its provenance verification.
.PHONY: engine bindings duckdb-ext third-party-licenses fr nl it rs es br us de de-recovery de-recovery-attempt-3 de-regress de-regress-dirty fr-poi mc smoke test test-py test-duckdb langchain-venv test-langchain http public-bench-contract public-bench

engine:
	cargo build --release --manifest-path gridpin/Cargo.toml
# Target-specific SHELL is inherited by every prerequisite, including engine/bindings.
# The immutable OS sandbox also follows recursive make and children that replace env/PATH.
# Keep the existing build/test recipes intact; the check uses only the already built CLI.
.PHONY: lint-fmt lint-clippy
# Match the pinned CI lint commands for both Rust packages. The lint cache must
# never share the measurement engine target directory. Cargo stays offline.
RUST_LINT_TARGET_DIR ?= $(CURDIR)/gridpin/target/ci-lint
lint-fmt:
	cargo fmt --check --manifest-path gridpin/Cargo.toml && \
	cargo fmt --check --manifest-path duckdb-ext/Cargo.toml

lint-clippy:
	CARGO_NET_OFFLINE=true CARGO_TARGET_DIR="$(RUST_LINT_TARGET_DIR)" \
		cargo clippy --manifest-path gridpin/Cargo.toml --all-targets --all-features -- -D warnings && \
	CARGO_NET_OFFLINE=true CARGO_TARGET_DIR="$(RUST_LINT_TARGET_DIR)" \
		cargo clippy --manifest-path duckdb-ext/Cargo.toml --all-targets -- -D warnings

# Everything runnable without private data: Rust unit tests, worker tests, Python
# bindings and DuckDB extension (against the Monaco smoke sheet). The full quality
# full quality check needs private sheets and lives in Makefile.lab.
test: engine
	cargo test --release --manifest-path gridpin/Cargo.toml
	cargo test --release --manifest-path duckdb-ext/Cargo.toml
	$(MAKE) test-py test-duckdb

# Python bindings against the Monaco smoke sheet (build it first: make mc)
test-py: bindings mc
	.venv-py/bin/python -m pytest gridpin/tests/ -q
	# gridpin-mcp: sheet downloads and agent tools; the MCP wiring tests skip without the mcp SDK
	.venv-py/bin/python -m pytest mcp/tests/ -q

# langchain-gridpin (integrations/langchain) against the Monaco smoke sheet, in its own venv:
# langchain-tests brings pytest plugins (asyncio, socket, recording) that must not load into
# .venv-py. The venv uses the published gridpin engine wheel, as the package's users do.
# -p no:benchmark: pytest-benchmark would leave a .benchmarks/ folder in code/ on every run.
LANGCHAIN_PY := .venv-langchain/bin/python
langchain-venv:
	python3 -m venv .venv-langchain
	$(LANGCHAIN_PY) -m pip install --quiet -r integrations/langchain/requirements-test.txt

test-langchain: mc
	test -x $(LANGCHAIN_PY) || { echo 'Run make langchain-venv first'; exit 1; }
	GRIDPIN_TEST_SHEET=$$PWD/data/mc.bin $(LANGCHAIN_PY) -m pytest integrations/langchain/tests -q -p no:cacheprovider -p no:benchmark

# DuckDB extension against the Monaco smoke sheet
test-duckdb: bindings mc
	test -s duckdb-ext/gridpin_ext.duckdb_extension || { echo 'Run make duckdb-ext first'; exit 1; }
	GRIDPIN_TEST_SHEET=$$PWD/data/mc.bin $(DUCKDB_PYTHON) -m pytest duckdb-ext/test/test_extension.py -q

# Rebuild the Python binding after engine changes. Rebuild the DuckDB extension
# explicitly with duckdb-ext; only an explicit request starts that source build.
# Prerequisite for Python: a venv with maturin at .venv-py/.
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

# CPP ABI requires DuckDB v1.5.5 sources and the same runtime version.
# Supply an existing source checkout/archive; this target never downloads it.
DUCKDB_SOURCE_DIR ?= $(CURDIR)/duckdb-ext/duckdb
DUCKDB_BUILD_DIR ?= $(CURDIR)/duckdb-ext/build/release
DUCKDB_CMAKE_FLAGS ?=
DUCKDB_BUILD_JOBS ?= 4
DUCKDB_PYTHON ?= .venv-py/bin/python
duckdb-ext:
	test -f "$(DUCKDB_SOURCE_DIR)/CMakeLists.txt" || { echo 'Set DUCKDB_SOURCE_DIR to DuckDB v1.5.5 sources'; exit 1; }
	cmake -S "$(DUCKDB_SOURCE_DIR)" -B "$(DUCKDB_BUILD_DIR)" -DCMAKE_BUILD_TYPE=Release -DBUILD_UNITTESTS=OFF -DBUILD_SHELL=ON -DOVERRIDE_GIT_DESCRIBE=v1.5.5 -DDUCKDB_EXTENSION_CONFIGS="$(CURDIR)/duckdb-ext/extension_config.cmake" $(DUCKDB_CMAKE_FLAGS)
	cmake --build "$(DUCKDB_BUILD_DIR)" --target gridpin_ext_loadable_extension --parallel $(DUCKDB_BUILD_JOBS)
	cp "$(DUCKDB_BUILD_DIR)/extension/gridpin_ext/gridpin_ext.duckdb_extension" duckdb-ext/gridpin_ext.duckdb_extension

# License texts of the Rust libraries compiled into the wheel, the CLI archives (gridpin) and the
# DuckDB extension (duckdb-ext). Regenerate and commit after every Cargo.lock change: the release
# workflow refuses a THIRDPARTY.yml that no longer matches its lockfile. The generator must see the
# same features as the build: pyo3 is optional behind `python` (the wheel's [tool.maturin]
# features), and without it six compiled-in libraries go missing. The CLI links a subset of the
# same list. Needs: cargo install cargo-bundle-licenses --version 4.2.0 --locked
third-party-licenses:
	cd gridpin && cargo bundle-licenses --features python --format yaml --output THIRDPARTY.yml
	cd duckdb-ext && cargo bundle-licenses --format yaml --output THIRDPARTY.yml
	python3 .github/thirdparty_licenses.py gridpin duckdb-ext

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

# Build from the accepted Spanish extract; source acquisition is a separate phase.
es: RULES=$(if $(wildcard rules),--rules rules/es,)
es: engine
	test -s data/es_norm.parquet
	$(if $(wildcard rules),python3 prep/es_rules.py,)
	nice -n 19 python3 prep/export_build.py data/es_norm.parquet data/build_es.csv.gz
	GRIDPIN_REQUIRE_META=1 nice -n 19 $(BIN) build data/build_es.csv.gz data/es.bin $(MODELS) --meta data/es_manifest.json

# Brazil (ANSWERS §41): Overture 2026-08-19.0, whole country, one sheet. The extraction
# streams from S3 in two passes (prep/overture.py BR) and never holds the 74M rows.
br: RULES=$(if $(wildcard rules),--rules rules/br,)
br: engine
	nice -n 19 python3 prep/overture.py BR
	$(if $(wildcard rules),python3 prep/br_rules.py,)
	GRIDPIN_MEM=8GB nice -n 19 python3 prep/export_build.py data/br_norm.parquet data/build_br.csv.gz
	GRIDPIN_REQUIRE_META=1 nice -n 19 $(BIN) build data/build_br.csv.gz data/br.bin $(MODELS) --meta data/br_manifest.json

# United States (ANSWERS §140): Overture 2026-09-23.1, only the 20 datasets with an explicit right
# to redistribute (prep/overture.py US_GREEN_DATASETS), 50 states + DC, one sheet marked as partial
# coverage. Streamed from S3 like BR; verbatim repeats are merged locally (us_stats: rows_duplicate).
# Rows without a city (about 4.7 M) get the nearest populated place of their state from GeoNames
# (CC BY 4.0) in a zero pass (us_stats: rows_nocity_src, umbrella_filled, umbrella_cross_state).
# Interpolation layer (ANSWERS §147): TIGER/Line ADDRFEAT address ranges of the Census Bureau
# (public domain) become marked range points outside the ZIPs the point sources cover
# (prep/us_tiger.py); the city of a range is the USPS place of its ZIP (GeoNames postal codes,
# CC BY 4.0). `make tiger-fetch` downloads the 3 227 county files (2.7 GB) into TIGER_DIR.
TIGER_YEAR?=2025
TIGER_DIR?=data/tiger_addrfeat
GEONAMES_POSTAL_US=data/geonames_postal_us.txt
tiger-fetch:
	mkdir -p $(TIGER_DIR)
	curl -s https://www2.census.gov/geo/tiger/TIGER$(TIGER_YEAR)/ADDRFEAT/ \
	  | grep -o 'tl_$(TIGER_YEAR)_[0-9]\{5\}_addrfeat\.zip' | sort -u \
	  | xargs -P 4 -I{} sh -c 'test -s $(TIGER_DIR)/{} || curl -s -f -o $(TIGER_DIR)/{}.part https://www2.census.gov/geo/tiger/TIGER$(TIGER_YEAR)/ADDRFEAT/{} && mv $(TIGER_DIR)/{}.part $(TIGER_DIR)/{}'
	ls $(TIGER_DIR)/tl_$(TIGER_YEAR)_*_addrfeat.zip | wc -l

us: RULES=$(if $(wildcard rules),--rules rules/us,)
us: engine
	test -f $(GEONAMES_US) || python3 prep/fetch_zip.py https://download.geonames.org/export/dump/US.zip US.txt $(GEONAMES_US)
	test -f $(GEONAMES_POSTAL_US) || python3 prep/fetch_zip.py https://download.geonames.org/export/zip/US.zip US.txt $(GEONAMES_POSTAL_US)
	nice -n 19 python3 prep/overture.py US $(GEONAMES_US)
	$(if $(wildcard rules),python3 prep/us_rules.py,)
	GRIDPIN_MEM=8GB nice -n 19 python3 prep/us_tiger.py $(TIGER_DIR) $(GEONAMES_POSTAL_US) data/us_norm.parquet data/us_tiger.parquet --manifest data/us_manifest.json
	GRIDPIN_MEM=8GB nice -n 19 python3 prep/export_build.py data/us_norm.parquet,data/us_tiger.parquet data/build_us.csv.gz
	GRIDPIN_REQUIRE_META=1 nice -n 19 $(BIN) build data/build_us.csv.gz data/us.bin $(MODELS) --meta data/us_manifest.json

rs: engine
	test -f $(GEONAMES_RS) || python3 prep/fetch_zip.py https://download.geonames.org/export/dump/RS.zip RS.txt $(GEONAMES_RS)
	nice -n 19 python3 prep/overture.py RS $(GEONAMES_RS)
	nice -n 19 python3 prep/export_build.py data/rs_norm.parquet data/build_rs.csv.gz
	GRIDPIN_REQUIRE_META=1 nice -n 19 $(BIN) build data/build_rs.csv.gz data/rs.bin $(MODELS) --meta data/rs_manifest.json

# Germany is intentionally not wired as three independently repeatable recipes.  Its
# release pin, shared cross-worktree lock, irrevocable receipt, local-toolchain
# preflight, run-scoped scratch and continuous disk checks live in one orchestrator.
# The target is one-shot: a receipt from an attempted run is never bypassed here.
DE_EXPECTED_SHA ?= $(shell git rev-parse --verify HEAD)
de: engine
	.venv-py/bin/python prep/de_build.py --expected-sha "$(DE_EXPECTED_SHA)"

# The ordinary target stays permanently blocked by attempt #1.  This distinct
# surface can consume only the fixed, owner-authorized attempt #2; there is no
# general attempt number, force, resume, or cleanup.
de-recovery: engine
	.venv-py/bin/python prep/de_build.py --recovery-attempt-2 --expected-sha "$(DE_EXPECTED_SHA)"

# A second fixed surface exists only for the separately authorized attempt #3.
# It has its own immutable receipt/auth/snapshot anchors and no attempt #4 form.
de-recovery-attempt-3: engine
	.venv-py/bin/python prep/de_build.py --recovery-attempt-3 --expected-sha "$(DE_EXPECTED_SHA)"

# Repeatable, offline product regression guard over the frozen Germany 5k.
# The output directory is content-addressed by track, HEAD, and shipping-sheet
# hash.  A repeated run is fail-closed unless the caller explicitly sets
# DE_REGRESS_FORCE=1; the manifest records that overwrite event.
DE_REGRESS_INDEX ?= data/de.bin
DE_REGRESS_BASELINE ?= eval/de_regress_baseline.json
DE_REGRESS_OUTPUT_ROOT ?= eval/work/de_regress
DE_REGRESS_TIMEOUT ?= 3600
DE_REGRESS_FORCE_ARG = $(if $(filter 1,$(DE_REGRESS_FORCE)),--force,)
de-regress: engine
	.venv-py/bin/python eval/de_5k_regress.py --track clean --gridpin-bin "$(BIN)" \
		--index "$(DE_REGRESS_INDEX)" --baseline "$(DE_REGRESS_BASELINE)" \
		--output-root "$(DE_REGRESS_OUTPUT_ROOT)" --timeout "$(DE_REGRESS_TIMEOUT)" \
		$(DE_REGRESS_FORCE_ARG) $(DASHBOARD_AFTER_MEASUREMENT)

de-regress-dirty: engine
	.venv-py/bin/python eval/de_5k_regress.py --track dirty --gridpin-bin "$(BIN)" \
		--index "$(DE_REGRESS_INDEX)" --baseline "$(DE_REGRESS_BASELINE)" \
		--output-root "$(DE_REGRESS_OUTPUT_ROOT)" --timeout "$(DE_REGRESS_TIMEOUT)" \
		$(DE_REGRESS_FORCE_ARG) $(DASHBOARD_AFTER_MEASUREMENT)

# Run before engine/rule commits, using the already built candidate (no rebuild).
WIKIDATA4_BIN ?= $(BIN)
WIKIDATA4_BASELINE ?= eval/wikidata4_regress_baseline.json
WIKIDATA4_CORPUS_ROOT ?= eval/wikidata4
WIKIDATA4_INDEX_DIR ?= data
WIKIDATA4_OUTPUT_ROOT ?= eval/work/wikidata4_regress
WIKIDATA4_TIMEOUT ?= 3600
WIKIDATA4_HEAD_ARG = $(if $(WIKIDATA4_EXPECTED_HEAD),--expected-head "$(WIKIDATA4_EXPECTED_HEAD)",)
WIKIDATA4_ENGINE_ARG = $(if $(WIKIDATA4_EXPECTED_ENGINE_SHA256),--expected-engine-sha256 "$(WIKIDATA4_EXPECTED_ENGINE_SHA256)",)
.PHONY: wikidata4-regress
wikidata4-regress:
	.venv-py/bin/python eval/wikidata4_regress.py --gridpin-bin "$(WIKIDATA4_BIN)" \
		--baseline "$(WIKIDATA4_BASELINE)" --corpus-root "$(WIKIDATA4_CORPUS_ROOT)" \
		--index-dir "$(WIKIDATA4_INDEX_DIR)" --output-root "$(WIKIDATA4_OUTPUT_ROOT)" \
		--timeout "$(WIKIDATA4_TIMEOUT)" $(WIKIDATA4_HEAD_ARG) $(WIKIDATA4_ENGINE_ARG) $(DASHBOARD_AFTER_MEASUREMENT)

# Five-country guard, sequential even under make -j; use the existing engine.
.PHONY: cross-regress
cross-regress:
	$(MAKE) -o engine de-regress
	$(MAKE) -o engine wikidata4-regress

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
