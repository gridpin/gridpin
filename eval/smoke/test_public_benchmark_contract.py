#!/usr/bin/env python3
"""Fail-closed contract tests for the public benchmark corpus and scorer."""

from __future__ import annotations

import argparse
import copy
import contextlib
import importlib.util
import io
import json
import pathlib
import tempfile
import urllib.parse


ROOT = pathlib.Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "examples" / "public_benchmark.py"
SPEC = importlib.util.spec_from_file_location("public_benchmark", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise SystemExit(f"cannot load {MODULE_PATH}")
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)


def _row(country: str, number: int, lineage_class: str | None = None) -> dict:
    record_id = f"public-{country}-{number}"
    if lineage_class is None:
        if number < BENCH.DEFAULT_UNKNOWN_MINIMUM:
            lineage_class = "unknown_lineage"
        elif number % 2:
            lineage_class = "outside_chain"
        else:
            lineage_class = "common_upstream"
    west, south, east, north = BENCH.COUNTRIES[country]["bounds"]
    row = {
        "schema": BENCH.TRUTH_SCHEMA,
        "country": country,
        "street_address": f"Public test street {number}",
        "postcode": f"{10_000 + number}",
        "municipality": f"Test municipality {number}",
        "lat": (south + north) / 2,
        "lon": (west + east) / 2 + number / 100_000.0,
        "record_id": record_id,
        "source_url": f"https://public-data.example/records/{record_id}",
        "source_release": BENCH.OVERTURE_RELEASE,
        "source_theme": BENCH.OVERTURE_THEME,
        "coordinate_provenance": {
            "source_name": "Public coordinate registry",
            "source_url": f"https://coordinates.example/records/{record_id}",
            "record_id": record_id,
            "retrieved_at": "2026-01-01T00:00:00+00:00",
            "license": "CC-BY-4.0",
            "common_ancestor": "OpenStreetMap" if lineage_class == "common_upstream" else None,
            "evidence_url": (
                f"https://evidence.example/records/{record_id}"
                if lineage_class == "outside_chain"
                else None
            ),
            "same_export_as_indexed_sheet": False,
        },
        "license": "CC-BY-4.0",
        "retrieved_at": "2026-01-01T00:00:00+00:00",
        "lineage_class": lineage_class,
    }
    row["query"] = BENCH._compose_query(row)
    return row


def _write(path: pathlib.Path, rows: list[dict]) -> None:
    path.write_text(
        "".join(json.dumps(row, sort_keys=True) + "\n" for row in rows),
        encoding="utf-8",
    )


V4_ASSEMBLED_AT = "2026-08-01T23:05:27Z"


def _v4_catalog() -> dict:
    catalog = copy.deepcopy(BENCH.V4_SOURCE_CATALOG)
    for entry in catalog.values():
        if entry["family"] != BENCH.OSM_SOURCE_FAMILY:
            entry["runtime"] = {"duckdb_version": "1.5.3"}
    return catalog


def _exercise_schema4_contract(work: pathlib.Path, base_rows: list[dict]) -> None:
    osm_source_by_country = {
        countries[0]: source_id
        for source_id, countries in BENCH.V4_SOURCE_COUNTRIES.items()
        if source_id != BENCH.OVERTURE_SOURCE_ID
    }
    rows: list[dict] = []
    for base in base_rows:
        row = copy.deepcopy(base)
        number = int(row["record_id"].rsplit("-", 1)[-1])
        source_id = (
            BENCH.OVERTURE_SOURCE_ID
            if number < 50
            else osm_source_by_country[row["country"]]
        )
        source = BENCH.V4_SOURCE_CATALOG[source_id]
        artifact = copy.deepcopy(source["retained_input"])
        row.update({
            "schema": BENCH.MULTI_SOURCE_TRUTH_SCHEMA,
            "lineage_class": "unknown_lineage",
            "truth_source_id": source_id,
            "truth_source_family": source["family"],
            "source_release": source["source_release"],
            "source_theme": source["theme"],
            "source_type": source["type"],
            "source_snapshot_at": source["snapshot_at"],
            "source_artifact": artifact,
            "source_sha256": artifact["sha256"],
            "lineage_policy": BENCH.LINEAGE_POLICY,
        })
        if source_id == BENCH.OVERTURE_SOURCE_ID:
            provenance = dict(row["coordinate_provenance"])
            provenance.update({
                "retrieved_at": source["snapshot_at"],
                "common_ancestor": None,
                "evidence_url": None,
                "source_family": source["family"],
                "source_snapshot_at": source["snapshot_at"],
                "snapshot_logical_name": artifact["logical_name"],
                "snapshot_sha256": artifact["sha256"],
                "acquisition_manifest_sha256": source["acquisition_manifest"]["sha256"],
            })
            row.update({
                "retrieved_at": source["snapshot_at"],
                "source_record_id": row["record_id"],
                "source_license": row["license"],
                "licenses": [row["license"]],
                "source_url": provenance["source_url"],
                "coordinate_provenance": provenance,
                "coordinate_source_dataset": [provenance["source_name"]],
                "coordinate_source_records": [{
                    "dataset": provenance["source_name"],
                    "record_id": provenance["record_id"],
                    "license": row["license"],
                }],
            })
        else:
            object_id = len(rows) + 1
            source_record_id = f"node/{object_id}"
            object_url = f"https://www.openstreetmap.org/{source_record_id}"
            row.update({
                "retrieved_at": V4_ASSEMBLED_AT,
                "license": source["license"],
                "source_license": source["license"],
                "licenses": [source["license"]],
                "source_record_id": source_record_id,
                "source_url": source["public_uri"],
                "coordinate_provenance": {
                    "source_name": source["dataset"],
                    "source_url": object_url,
                    "record_id": source_record_id,
                    "retrieved_at": V4_ASSEMBLED_AT,
                    "license": source["license"],
                    "common_ancestor": None,
                    "evidence_url": object_url,
                    "same_export_as_indexed_sheet": False,
                    "source_family": source["family"],
                    "source_snapshot_at": source["snapshot_at"],
                    "snapshot_logical_name": artifact["logical_name"],
                    "snapshot_sha256": artifact["sha256"],
                    "coordinate_method": "node_location",
                    "object_type": "node",
                    "object_id": object_id,
                    "replication_base_url": source["pbf"]["replication_base_url"],
                    "replication_sequence": source["pbf"]["replication_sequence"],
                    "attribution_url": source["copyright_url"],
                },
            })
        rows.append(row)

    path = work / "schema4.jsonl"
    _write(path, rows)
    by_country, by_lineage, by_country_and_lineage = BENCH._lineage_counts(rows)
    source_ids = tuple(sorted(BENCH.V4_SOURCE_CATALOG))
    by_source, by_country_and_source, by_country_source_lineage = BENCH._source_counts(
        rows, source_ids
    )
    manifest = {
        "schema": BENCH.MULTI_SOURCE_TRUTH_SCHEMA,
        "sha256": BENCH._sha256(path),
        "rows": len(rows),
        "rows_by_country": by_country,
        "rows_by_lineage": by_lineage,
        "rows_by_country_and_lineage": by_country_and_lineage,
        "rows_by_source": by_source,
        "rows_by_country_and_source": by_country_and_source,
        "rows_by_country_source_and_lineage": by_country_source_lineage,
        "assembled_at": V4_ASSEMBLED_AT,
        "licenses": sorted({row["license"] for row in rows}),
        "lineage_policy": BENCH.LINEAGE_POLICY,
        "source_catalog": _v4_catalog(),
        "recipe": {
            "script": "examples/hybrid_truth_corpus.py",
            "script_sha256": "a" * 64,
            "command": "python3 examples/hybrid_truth_corpus.py --fixed-inputs",
        },
    }
    manifest_path = BENCH._truth_manifest_path(path)
    BENCH._atomic_json(manifest_path, manifest)
    validated = BENCH.validate_truth(path)
    assert len(validated) == 1_200
    assert BENCH._validate_truth_manifest(path, validated) == manifest
    score = BENCH._score(
        validated,
        [(row["lat"], row["lon"]) for row in validated],
        BENCH.DEFAULT_DISTANCE_M,
    )
    assert score["overall"]["interpretation"] == "descriptive_only"
    assert score["sources"][BENCH.OVERTURE_SOURCE_ID]["cases"] == 200
    relationships = BENCH._source_relationships(
        ("gridpin", "photon", "nominatim"), manifest["source_catalog"]
    )
    osm_id = "osm_geofabrik_fr_alsace_260701"
    assert relationships["photon"][osm_id]["relationship"] == "same_dataset_family"
    assert relationships["gridpin"][osm_id]["relationship"] == "unknown"

    mutated = list(rows)
    mutated[50] = copy.deepcopy(mutated[50])
    mutated[50]["coordinate_provenance"]["snapshot_sha256"] = "f" * 64
    mutation_path = work / "schema4-row-mutation.jsonl"
    _write(mutation_path, mutated)
    _must_fail(mutation_path, "snapshot_sha256")

    wrong_counts = copy.deepcopy(manifest)
    wrong_counts["rows_by_source"][osm_id] -= 1
    BENCH._atomic_json(manifest_path, wrong_counts)
    try:
        BENCH._validate_truth_manifest(path, validated)
    except BENCH.BenchmarkError as exc:
        assert "rows_by_source" in str(exc), exc
    else:
        raise AssertionError("schema-4 source count mutation passed")
    BENCH._atomic_json(manifest_path, manifest)

    final = (
        ROOT / "public-bench-work" / "bl14"
        / "hybrid-truth-corpus-v4-final-20260801T230527Z.jsonl"
    )
    if final.is_file():
        final_rows = BENCH.validate_truth(final)
        BENCH._validate_truth_manifest(final, final_rows)
        assert BENCH._sha256(final) == (
            "d62033a60c434fe1d9a8937681cb014e8d2f75bccb934df465a791dda227433f"
        )


def _write_status_evidence(path: pathlib.Path, engine: str, query_endpoint: str) -> None:
    if engine == "photon":
        response = {
            "status": "Ok",
            "version": "0.7.0",
            "git_commit": "abc123",
            "import_date": "2026-01-01T00:00:00Z",
        }
    else:
        response = {
            "status": 0,
            "software_version": "5.1.0",
            "database_version": "5.1.0-0",
            "data_updated_at": "2026-01-01T00:00:00Z",
        }
    BENCH._atomic_json(path, {
        "schema": BENCH.STATUS_EVIDENCE_SCHEMA,
        "engine": engine,
        "endpoint": BENCH._status_endpoint(engine, query_endpoint),
        "fetched_at": "2026-01-02T00:00:00+00:00",
        "response": response,
    })


def _must_fail(path: pathlib.Path, phrase: str) -> None:
    try:
        BENCH.validate_truth(path)
    except BENCH.BenchmarkError as exc:
        if phrase not in str(exc):
            raise AssertionError(f"wrong failure for {path}: {exc}") from exc
    else:
        raise AssertionError(f"invalid corpus passed: {path}")


def _must_parser_fail(parser, payload: object) -> None:
    try:
        parser(payload)
    except BENCH.BenchmarkError:
        pass
    else:
        raise AssertionError(f"invalid response schema passed: {payload!r}")


def _write_fake_gridpin(path: pathlib.Path) -> None:
    bounds = {country: cfg["bounds"] for country, cfg in BENCH.COUNTRIES.items()}
    script = f"""#!{pathlib.Path('/usr/bin/env')} python3
import json
import pathlib
import sys

BOUNDS = {bounds!r}

if sys.argv[1] == "meta":
    country = pathlib.Path(sys.argv[2]).read_text(encoding="utf-8").strip()
    print(json.dumps({{
        "country": country.lower(),
        "layer": "addresses",
        "source_release": "2026-01",
        "license": "fixture license",
        "sources": "Overture Maps addresses via OpenAddresses",
    }}))
elif sys.argv[1] == "batch":
    sheet, source, destination = map(pathlib.Path, sys.argv[2:5])
    country = sheet.read_text(encoding="utf-8").strip()
    west, south, east, north = BOUNDS[country]
    with source.open(encoding="utf-8") as incoming, destination.open("w", encoding="utf-8") as outgoing:
        for line in incoming:
            query = json.loads(line)["q"]
            number = int(query.split()[3].rstrip(","))
            payload = {{"results": [{{
                "lat": (south + north) / 2,
                "lon": (west + east) / 2 + number / 100_000.0,
            }}]}}
            outgoing.write(json.dumps(payload) + "\\n")
else:
    raise SystemExit(2)
"""
    path.write_text(script, encoding="utf-8")
    path.chmod(0o755)


class _Response(io.BytesIO):
    status = 200

    def __init__(self, body: bytes, final_url: str):
        super().__init__(body)
        self._final_url = final_url

    def geturl(self) -> str:
        return self._final_url

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()


def _exercise_end_to_end(work: pathlib.Path, truth: pathlib.Path, rows: list[dict]) -> None:
    binary = work / "fake-gridpin"
    _write_fake_gridpin(binary)
    sheets: list[str] = []
    sheet_paths: dict[str, pathlib.Path] = {}
    for country in BENCH.COUNTRIES:
        sheet = work / f"{country.lower()}.bin"
        sheet.write_text(country, encoding="utf-8")
        sheet_paths[country] = sheet
        sheets.append(f"{country}={sheet}")

    requests: list[str] = []
    rows_by_query = {row["query"]: row for row in rows}

    def fake_urlopen(request, timeout):
        assert timeout == 2
        requests.append(request.full_url)
        parsed = urllib.parse.urlsplit(request.full_url)
        params = urllib.parse.parse_qs(parsed.query)
        query = params["q"][0]
        row = rows_by_query[query]
        lat, lon = row["lat"], row["lon"]
        if parsed.hostname == "photon.test":
            assert params["countrycode"] == [row["country"]]
            payload = {"features": [{"geometry": {"coordinates": [lon, lat]}}]}
        elif parsed.hostname == "nominatim.test":
            assert params["countrycodes"] == [row["country"].lower()]
            payload = [{"lat": str(lat), "lon": str(lon)}]
        else:
            raise AssertionError(f"unexpected endpoint: {request.full_url}")
        return _Response(json.dumps(payload).encode("utf-8"), request.full_url)

    original_urlopen = BENCH._urlopen_no_redirect
    BENCH._urlopen_no_redirect = fake_urlopen
    photon_evidence = work / "photon-status.json"
    nominatim_evidence = work / "nominatim-status.json"
    _write_status_evidence(photon_evidence, "photon", "http://photon.test/api/")
    _write_status_evidence(
        nominatim_evidence, "nominatim", "http://nominatim.test/search"
    )
    args = argparse.Namespace(
        truth=truth,
        gridpin_bin=binary,
        sheet=sheets,
        competitor=["photon", "nominatim"],
        photon_url="http://photon.test",
        photon_status_evidence=photon_evidence,
        photon_not_run_reason=None,
        nominatim_url="http://nominatim.test",
        nominatim_status_evidence=nominatim_evidence,
        nominatim_not_run_reason=None,
        allow_public_services=False,
        user_agent="GridPin-public-benchmark/1.0 (quality@example.com)",
        minimum=BENCH.DEFAULT_MINIMUM,
        unknown_minimum=BENCH.DEFAULT_UNKNOWN_MINIMUM,
        distance_m=BENCH.DEFAULT_DISTANCE_M,
        pause=0,
        timeout=2,
        work=work / "cache",
        output=work / "results.json",
    )
    try:
        with contextlib.redirect_stdout(io.StringIO()):
            BENCH.command_run(args)
        result = json.loads(args.output.read_text(encoding="utf-8"))
        assert len(requests) == len(rows) * 2
        assert all(
            result["results"][engine]["overall"]["hit_at_1_pct"] == 100.0
            for engine in ("gridpin", "photon", "nominatim")
        )
        assert all(
            result["results"][engine]["country_lineages"][country]["unknown_lineage"]
            ["hit_at_1_pct"] == 100.0
            for engine in ("gridpin", "photon", "nominatim")
            for country in BENCH.COUNTRIES
        )
        assert result["not_run"] == {}
        assert result["endpoints"]["photon"]["version"] == "0.7.0"
        assert (
            result["endpoints"]["photon"]["attribution"]
            == BENCH.SERVICE_ATTRIBUTION["photon"]
        )
        assert (
            result["endpoints"]["photon"]["status_evidence"]["sha256"]
            == BENCH._sha256(photon_evidence)
        )
        assert (
            result["results"]["photon"]["status_evidence_sha256"]
            == BENCH._sha256(photon_evidence)
        )
        assert set(result["gridpin_artifacts"]["sheets"]) == set(BENCH.COUNTRIES)
        assert all(
            result["gridpin_artifacts"]["sheets"][country]["meta"]["country"]
            == country.lower()
            for country in BENCH.COUNTRIES
        )
        assert all(
            set(result["gridpin_artifacts"]["sheets"][country]["meta"])
            == set(BENCH.SHEET_META_ALLOWLIST)
            for country in BENCH.COUNTRIES
        )
        assert all(
            result["response_caches"][engine]["rows"] == len(rows)
            and len(result["response_caches"][engine]["sha256"]) == 64
            for engine in ("photon", "nominatim")
        )
        assert result["truth"]["source_details"] == {
            "dataset": BENCH.OVERTURE_DATASET,
            "theme": BENCH.OVERTURE_THEME,
            "type": BENCH.OVERTURE_TYPE,
            "source_release": BENCH.OVERTURE_RELEASE,
            "uri": BENCH.OVERTURE_S3,
            "retrieved_at": "2026-01-01T00:00:00+00:00",
            "license": "MIXED: row-level coordinate source licenses",
        }
        assert result["source_separation"]["same_export_as_indexed_sheet"] is False
        assert all(
            evidence["overture_sheet_sources_explicitly_addresses"] is True
            and evidence["sheet_sources_match_truth_theme_or_type"] is False
            for evidence in result["source_separation"]["sheets"].values()
        )

        # A second complete run must consume the endpoint/corpus-bound raw caches
        # and therefore remain runnable with the network observer disabled.
        BENCH._urlopen_no_redirect = lambda *_args, **_kwargs: (_ for _ in ()).throw(
            AssertionError("complete cache attempted network access")
        )
        args.output = work / "replayed-results.json"
        with contextlib.redirect_stdout(io.StringIO()):
            BENCH.command_run(args)

        # A fresh result path remains absent when a selected endpoint fails;
        # a prior successful result at a different immutable path is untouched.
        failing_evidence = work / "photon-status-failing-run.json"
        _write_status_evidence(failing_evidence, "photon", "http://photon.test/api/")
        failing_payload = json.loads(failing_evidence.read_text(encoding="utf-8"))
        failing_payload["fetched_at"] = "2026-01-03T00:00:00+00:00"
        BENCH._atomic_json(failing_evidence, failing_payload)
        args.competitor = ["photon"]
        args.photon_status_evidence = failing_evidence
        args.nominatim_not_run_reason = "not selected for forced failure observer"
        args.output = work / "failed-endpoint-results.json"
        BENCH._urlopen_no_redirect = lambda *_args, **_kwargs: (_ for _ in ()).throw(
            OSError("forced endpoint failure")
        )
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                BENCH.command_run(args)
        except BENCH.BenchmarkError as exc:
            assert "request failed" in str(exc)
        else:
            raise AssertionError("endpoint failure produced a result")
        assert not args.output.exists()
        assert (work / "results.json").is_file()

        # A disk-limited run can select one competitor without publishing a
        # stale or synthetic score for the service that was not started.
        BENCH._urlopen_no_redirect = lambda *_args, **_kwargs: (_ for _ in ()).throw(
            AssertionError("complete cache attempted network access")
        )
        args.photon_status_evidence = photon_evidence
        args.competitor = ["photon"]
        args.nominatim_not_run_reason = "disk guard: Nominatim was not started"
        args.output = work / "photon-only-results.json"
        with contextlib.redirect_stdout(io.StringIO()):
            BENCH.command_run(args)
        one_service = json.loads(args.output.read_text(encoding="utf-8"))
        assert set(one_service["results"]) == {"gridpin", "photon"}
        assert "nominatim" not in one_service["endpoints"]
        assert one_service["not_run"]["nominatim"] == {
            "status": "not_run",
            "reason": "disk guard: Nominatim was not started",
        }

        args.competitor = ["photon", "nominatim"]
        args.nominatim_not_run_reason = None
        args.output = work / "corrupt-cache-results.json"

        # Cache schema drift is fail-closed rather than silently scored.
        photon_cache = next(args.work.glob("photon-*.jsonl"))
        cache_rows = BENCH._load_jsonl(photon_cache)
        cache_rows[0]["schema"] = 1
        BENCH._atomic_jsonl(photon_cache, cache_rows)
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                BENCH.command_run(args)
        except BENCH.BenchmarkError as exc:
            assert "cache mismatch for schema" in str(exc)
        else:
            raise AssertionError("invalid cache schema passed")

        # Embedded sheet identity is checked before a cached score can publish.
        args.output = work / "wrong-sheet-results.json"
        sheet_paths["FR"].write_text("IT", encoding="utf-8")
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                BENCH.command_run(args)
        except BENCH.BenchmarkError as exc:
            assert "wrong GridPin sheet identity" in str(exc)
        else:
            raise AssertionError("wrong GridPin sheet identity passed")
    finally:
        BENCH._urlopen_no_redirect = original_urlopen


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="gridpin-public-bench-contract-") as raw:
        work = pathlib.Path(raw)
        full_rows = [
            _row(country, number)
            for country in BENCH.COUNTRIES
            for number in range(BENCH.DEFAULT_MINIMUM)
        ]
        full = work / "full.jsonl"
        _write(full, full_rows)
        validated = BENCH.validate_truth(full)
        assert len(validated) == 4 * BENCH.DEFAULT_MINIMUM
        BENCH._atomic_json(BENCH._truth_manifest_path(full), {
            "schema": BENCH.TRUTH_SCHEMA,
            "sha256": BENCH._sha256(full),
            "rows": len(full_rows),
            "rows_by_country": {
                country: BENCH.DEFAULT_MINIMUM for country in BENCH.COUNTRIES
            },
            "rows_by_lineage": {
                "outside_chain": 4 * 75,
                "common_upstream": 4 * 75,
                "unknown_lineage": 4 * BENCH.DEFAULT_UNKNOWN_MINIMUM,
            },
            "rows_by_country_and_lineage": {
                country: {
                    "outside_chain": 75,
                    "common_upstream": 75,
                    "unknown_lineage": BENCH.DEFAULT_UNKNOWN_MINIMUM,
                }
                for country in BENCH.COUNTRIES
            },
            "retrieved_at": "2026-01-01T00:00:00+00:00",
            "source": "https://public-data.example",
            "licenses": ["CC-BY-4.0"],
            "lineage_policy": BENCH.LINEAGE_POLICY,
            "source_details": {
                "dataset": BENCH.OVERTURE_DATASET,
                "theme": BENCH.OVERTURE_THEME,
                "type": BENCH.OVERTURE_TYPE,
                "source_release": BENCH.OVERTURE_RELEASE,
                "uri": BENCH.OVERTURE_S3,
                "retrieved_at": "2026-01-01T00:00:00+00:00",
                "license": "MIXED: row-level coordinate source licenses",
            },
            "recipe": {
                "script": "overture_places_corpus.py",
                "script_sha256": "a" * 64,
                "command": "python examples/overture_places_corpus.py --acknowledge-public-s3-scan",
            },
        })

        _exercise_schema4_contract(work, full_rows)

        empty = work / "empty.jsonl"
        empty.write_text("", encoding="utf-8")
        _must_fail(empty, "truncated")

        truncated = work / "truncated.jsonl"
        _write(truncated, full_rows[:-1])
        _must_fail(truncated, "truncated")

        duplicate = work / "duplicate.jsonl"
        _write(duplicate, full_rows + [full_rows[0]])
        _must_fail(duplicate, "duplicate")

        missing_license_rows = list(full_rows)
        missing_license_rows[0] = dict(missing_license_rows[0], license="")
        missing_license = work / "missing-license.jsonl"
        _write(missing_license, missing_license_rows)
        _must_fail(missing_license, "license")

        for name, first_row, phrase in (
            (
                "missing-schema",
                {key: value for key, value in full_rows[0].items() if key != "schema"},
                "missing fields: schema",
            ),
            ("wrong-schema", dict(full_rows[0], schema=2), "schema must be the integer 3"),
            (
                "missing-release",
                {
                    key: value
                    for key, value in full_rows[0].items()
                    if key != "source_release"
                },
                "missing fields: source_release",
            ),
            (
                "non-utc-timestamp",
                dict(full_rows[0], retrieved_at="2026-01-01T01:00:00+01:00"),
                "ISO8601 UTC",
            ),
            (
                "coordinate-license-mismatch",
                dict(
                    full_rows[0],
                    coordinate_provenance=dict(
                        full_rows[0]["coordinate_provenance"], license="ODbL-1.0"
                    ),
                ),
                "must equal row license",
            ),
        ):
            mutated_rows = list(full_rows)
            mutated_rows[0] = first_row
            mutation_path = work / f"{name}.jsonl"
            _write(mutation_path, mutated_rows)
            _must_fail(mutation_path, phrase)

        for name, field, value, phrase in (
            ("wrong-release", "source_release", "2026-06-18.0", "source_release"),
            ("wrong-theme", "source_theme", "addresses", "theme"),
        ):
            mutated_rows = list(full_rows)
            mutated_rows[0] = dict(full_rows[0], **{field: value})
            mutation_path = work / f"{name}.jsonl"
            _write(mutation_path, mutated_rows)
            try:
                BENCH._validate_truth_manifest(full, BENCH.validate_truth(mutation_path))
            except BENCH.BenchmarkError as exc:
                assert phrase in str(exc), exc
            else:
                raise AssertionError(f"manifest/row {field} mismatch passed")

        manifest_path = BENCH._truth_manifest_path(full)
        valid_manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        missing_source_details = dict(valid_manifest)
        missing_source_details.pop("source_details")
        BENCH._atomic_json(manifest_path, missing_source_details)
        try:
            BENCH._validate_truth_manifest(full, validated)
        except BENCH.BenchmarkError as exc:
            assert "source_details" in str(exc), exc
        else:
            raise AssertionError("manifest without source_details passed")
        BENCH._atomic_json(manifest_path, valid_manifest)

        missing_municipality_rows = list(full_rows)
        missing_municipality_rows[0] = dict(missing_municipality_rows[0], municipality="")
        missing_municipality = work / "missing-municipality.jsonl"
        _write(missing_municipality, missing_municipality_rows)
        _must_fail(missing_municipality, "municipality")

        missing_source_rows = list(full_rows)
        provenance = dict(missing_source_rows[0]["coordinate_provenance"])
        provenance.pop("source_url")
        missing_source_rows[0] = dict(
            missing_source_rows[0], coordinate_provenance=provenance
        )
        missing_source = work / "missing-coordinate-source.jsonl"
        _write(missing_source, missing_source_rows)
        _must_fail(missing_source, "coordinate_provenance missing fields")

        weak_rows = list(full_rows)
        weak_rows[0] = _row("FR", 0, "outside_chain")
        weak = work / "weak-lineage.jsonl"
        _write(weak, weak_rows)
        _must_fail(weak, "unknown_lineage")

        common_rows = list(full_rows)
        common_rows[0] = _row("FR", 0, "common_upstream")
        common_provenance = dict(
            common_rows[0]["coordinate_provenance"], common_ancestor=None
        )
        common_rows[0] = dict(
            common_rows[0], coordinate_provenance=common_provenance
        )
        common = work / "common-without-ancestor.jsonl"
        _write(common, common_rows)
        _must_fail(common, "common_ancestor")

        # Missing/non-list containers and invalid top-1 coordinates are errors;
        # an explicitly present empty result list is the only honest empty hit.
        assert BENCH._gridpin_top1({"results": []}) is None
        assert BENCH._photon_top1({"features": []}) is None
        assert BENCH._nominatim_top1([]) is None
        for parser, payload in (
            (BENCH._gridpin_top1, {}),
            (BENCH._gridpin_top1, {"results": {}}),
            (BENCH._gridpin_top1, {"results": [{"lat": "nan", "lon": 2}]}),
            (BENCH._gridpin_top1, {"results": [{"lat": 91, "lon": 2}]}),
            (BENCH._photon_top1, {}),
            (BENCH._photon_top1, {"features": {}}),
            (BENCH._photon_top1, {"features": [{"geometry": {"coordinates": [2, "inf"]}}]}),
            (BENCH._photon_top1, {"features": [{"geometry": {"coordinates": [181, 48]}}]}),
            (BENCH._nominatim_top1, {}),
            (BENCH._nominatim_top1, [{"lat": 48, "lon": "nan"}]),
            (BENCH._nominatim_top1, [{"lat": -91, "lon": 2}]),
        ):
            _must_parser_fail(parser, payload)

        photon_url = BENCH._request_url(
            "photon", "http://photon.test/api/", full_rows[0]
        )
        assert urllib.parse.parse_qs(urllib.parse.urlsplit(photon_url).query)["countrycode"] == ["FR"]
        assert not BENCH._contains_normalized_token_sequence(
            "NotOpenAddresses", "OpenAddresses"
        )
        assert not BENCH._contains_normalized_token_sequence(
            "NotFoursquare", "Foursquare"
        )
        assert not BENCH._contains_normalized_token_sequence("urban", "BAN")
        try:
            BENCH._sheet_source_separation(
                "FR",
                {"layer": "addresses", "sources": "Overture Maps places"},
                valid_manifest["source_details"],
            )
        except BENCH.BenchmarkError as exc:
            assert "same truth theme/type" in str(exc), exc
        else:
            raise AssertionError("same Overture Places source passed separation gate")

        parser = BENCH.build_parser()
        subcommands = next(
            action for action in parser._actions if isinstance(action, argparse._SubParsersAction)
        )
        run_parser = subcommands.choices["run"]
        run_actions = {option: action for action in run_parser._actions for option in action.option_strings}
        assert not run_actions["--photon-url"].required and run_actions["--photon-url"].default is None
        assert not run_actions["--nominatim-url"].required and run_actions["--nominatim-url"].default is None
        assert run_actions["--competitor"].default is None
        assert run_actions["--allow-public-services"].default is False

        _exercise_end_to_end(work, full, full_rows)

    BENCH.command_self_test(None)
    print("public benchmark contract: PASS")


if __name__ == "__main__":
    main()
