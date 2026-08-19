#!/usr/bin/env python3
"""Validate and score the fixed schema-5 multi-region corpus with GridPin.

This intentionally small runner has no competitor/network mode.  It validates
the corpus/manifest/region coupling, runs one GridPin batch per country sheet,
and reports country scores, direct-OSM region scores and their within-country
spread.  Overture countrywide rows are always reported separately.  Every
result is structurally descriptive-only and headline-ineligible.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import importlib.util
import json
import math
import pathlib
import sys
from typing import Mapping, Sequence


EXAMPLES = pathlib.Path(__file__).resolve().parent


def _load_helper(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, EXAMPLES / filename)
    if spec is None or spec.loader is None:  # pragma: no cover
        raise RuntimeError(f"cannot load {filename}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


corpus = _load_helper("multiregion_corpus_contract", "multiregion_truth_corpus.py")
public = _load_helper("multiregion_public_benchmark_helpers", "public_benchmark.py")
BenchmarkError = public.BenchmarkError
RESULT_SCHEMA = 5
MAX_DISTANCE_M = 300.0


def _manifest_path(path: pathlib.Path) -> pathlib.Path:
    return path.with_suffix(path.suffix + ".manifest.json")


def _read_json(path: pathlib.Path, label: str) -> dict:
    payload, _ = corpus.overture._read_regular_bytes(path, label)
    try:
        value = json.loads(
            payload,
            parse_constant=lambda token: (_ for _ in ()).throw(
                ValueError(f"non-finite JSON constant {token}")
            ),
        )
    except (json.JSONDecodeError, ValueError) as exc:
        raise BenchmarkError(f"invalid {label} JSON: {path}") from exc
    if not isinstance(value, dict):
        raise BenchmarkError(f"{label} must be an object")
    return value


def _read_jsonl(path: pathlib.Path, label: str) -> list[dict]:
    payload, _ = corpus.overture._read_regular_bytes(path, label)
    rows: list[dict] = []
    for line_no, line in enumerate(payload.splitlines(), 1):
        try:
            row = json.loads(
                line,
                parse_constant=lambda token: (_ for _ in ()).throw(
                    ValueError(f"non-finite JSON constant {token}")
                ),
            )
        except (json.JSONDecodeError, ValueError) as exc:
            raise BenchmarkError(f"invalid {label} JSON at line {line_no}") from exc
        if not isinstance(row, dict):
            raise BenchmarkError(f"{label} line {line_no} is not an object")
        rows.append(row)
    return rows


def _require_subset(actual: object, expected: object, label: str) -> None:
    if isinstance(expected, dict):
        if not isinstance(actual, dict):
            raise BenchmarkError(f"{label} must be an object")
        for key, value in expected.items():
            if key not in actual:
                raise BenchmarkError(f"{label} is missing {key}")
            _require_subset(actual[key], value, f"{label}.{key}")
    elif actual != expected:
        raise BenchmarkError(f"{label} disagrees with the exact schema-5 contract")


def _expected_region_counts() -> dict[str, int]:
    expected = {
        corpus.COUNTRYWIDE_REGION_IDS[country]: corpus.OVERTURE_QUOTA
        for country in corpus.COUNTRIES
    }
    expected.update({region_id: int(region["quota"]) for region_id, region in corpus.REGION_CATALOG.items()})
    return dict(sorted(expected.items()))


def validate_truth(path: pathlib.Path) -> tuple[list[dict], dict]:
    """Validate the exact schema-5 corpus and return rows plus its manifest."""

    path = path.expanduser().resolve()
    truth_capture = corpus.hybrid._capture_streaming(path, "schema-5 truth corpus")
    manifest_capture = corpus.hybrid._capture_streaming(
        _manifest_path(path), "schema-5 truth manifest"
    )
    rows = _read_jsonl(path, "schema-5 truth corpus")
    manifest = _read_json(_manifest_path(path), "schema-5 truth manifest")
    corpus.hybrid._verify_capture(truth_capture, "schema-5 truth corpus")
    corpus.hybrid._verify_capture(manifest_capture, "schema-5 truth manifest")
    if (
        manifest.get("schema") != corpus.TRUTH_SCHEMA
        or manifest.get("kind") != corpus.MANIFEST_KIND
        or manifest.get("rows") != 1_200
        or len(rows) != 1_200
    ):
        raise BenchmarkError("truth corpus is not the exact 1,200-row schema-5 contract")
    if manifest.get("sha256") != truth_capture["sha256"]:
        raise BenchmarkError("truth corpus SHA-256 disagrees with its manifest")
    if manifest.get("region_catalog") != corpus.REGION_CATALOG:
        raise BenchmarkError("truth manifest region catalog changed")
    countrywide = manifest.get("countrywide_sampling")
    if not isinstance(countrywide, dict) or set(countrywide) != set(corpus.COUNTRIES):
        raise BenchmarkError("truth manifest has no exact countrywide sampling catalog")
    source_catalog = manifest.get("source_catalog")
    if not isinstance(source_catalog, dict):
        raise BenchmarkError("truth manifest has no source catalog")
    for source in corpus.OSM_SOURCE_CATALOG.values():
        source_id = str(source["source_id"])
        if source_id not in source_catalog:
            raise BenchmarkError(f"truth manifest is missing pinned source {source_id}")
        _require_subset(source_catalog[source_id], source, f"source_catalog.{source_id}")
    if corpus.hybrid.OVERTURE_SOURCE_ID not in source_catalog:
        raise BenchmarkError("truth manifest is missing the pinned Overture source")
    _require_subset(
        source_catalog[corpus.hybrid.OVERTURE_SOURCE_ID],
        corpus.OVERTURE_SOURCE_PIN,
        f"source_catalog.{corpus.hybrid.OVERTURE_SOURCE_ID}",
    )
    selection = manifest.get("selection")
    expected_selection = {
        "countrywide_overture_quota": corpus.OVERTURE_QUOTA,
        "direct_osm_role_quotas": corpus.ROLE_QUOTAS,
        "outside_chain_minimum_by_country": corpus.OUTSIDE_MINIMA,
        "outside_chain_minimum_total": corpus.TOTAL_OUTSIDE_MINIMUM,
        "candidate_multiplier": corpus.CANDIDATE_MULTIPLIER,
        "direct_region_diversity_caps": corpus.DIRECT_REGION_CAPS,
        "direct_region_municipality_policy": corpus.DIRECT_REGION_POLICY,
        "engine_blind": True,
    }
    _require_subset(selection, expected_selection, "selection")
    low_memory = manifest.get("low_memory_contract")
    _require_subset(low_memory, {
        "one_process_per_region_pbf": True,
        "global_advisory_lock": {
            "logical_name": corpus.GLOBAL_EXTRACTION_LOCK_NAME,
            "mode": "fcntl.flock LOCK_EX|LOCK_NB",
        },
        "threads": 1,
        "memory_limit": corpus.DUCKDB_MEMORY_LIMIT,
        "fetch_rows": corpus.MAX_FETCH_ROWS,
        "full_stream_collision_detection": True,
    }, "low_memory_contract")
    collision_diagnostics = manifest.get("collision_diagnostics")
    _require_subset(collision_diagnostics, {
        "overture_candidate_universe_rows": corpus.EXPECTED_OVERTURE_CANDIDATES,
        "policy": "exclude both OSM row and every matching Overture id before bounded selection",
    }, "collision_diagnostics")
    fragments = manifest.get("fragment_manifests")
    if not isinstance(fragments, dict) or set(fragments) != set(corpus.REGION_CATALOG):
        raise BenchmarkError("truth manifest does not bind exactly twelve region fragments")
    for region_id, fragment in fragments.items():
        if not isinstance(fragment, dict):
            raise BenchmarkError(f"fragment manifest pin {region_id} is not an object")
        for field in ("sha256", "fragment_sha256"):
            if not isinstance(fragment.get(field), str) or not re_full_sha(fragment[field]):
                raise BenchmarkError(f"fragment manifest pin {region_id} has invalid {field}")
    expected_counts = _expected_region_counts()
    if manifest.get("rows_by_sampling_region") != expected_counts:
        raise BenchmarkError("manifest sampling-region quotas changed")
    expected_country_counts = {
        country: {
            region_id: count
            for region_id, count in expected_counts.items()
            if region_id == corpus.COUNTRYWIDE_REGION_IDS[country]
            or corpus.REGION_CATALOG.get(region_id, {}).get("country") == country
        }
        for country in corpus.COUNTRIES
    }
    if manifest.get("rows_by_country_and_sampling_region") != expected_country_counts:
        raise BenchmarkError("manifest country/region count matrix changed")
    by_region: collections.Counter[str] = collections.Counter()
    by_country: collections.Counter[str] = collections.Counter()
    outside_by_country: collections.Counter[str] = collections.Counter()
    seen_ids: set[str] = set()
    seen_queries: set[str] = set()
    seen_coordinates: set[tuple[float, float]] = set()
    for line_no, row in enumerate(rows, 1):
        country = row.get("country")
        if row.get("schema") != corpus.TRUTH_SCHEMA or country not in corpus.COUNTRIES:
            raise BenchmarkError(f"truth row {line_no} has invalid schema/country")
        region_id = row.get("sampling_region_id")
        role = row.get("sampling_region_role")
        family = row.get("truth_source_family")
        try:
            lat, lon = float(row["lat"]), float(row["lon"])
        except (KeyError, TypeError, ValueError, OverflowError) as exc:
            raise BenchmarkError(f"truth row {line_no} has invalid coordinates") from exc
        if (
            not math.isfinite(lat)
            or not math.isfinite(lon)
            or not corpus.overture._finite_in_bounds(str(country), lat, lon)
        ):
            raise BenchmarkError(f"truth row {line_no} has non-finite/out-of-country coordinates")
        if not isinstance(row.get("query"), str) or not row["query"].strip():
            raise BenchmarkError(f"truth row {line_no} has no query")
        if (
            row.get("lineage_class") not in corpus.hybrid.LINEAGE_CLASSES
            or row.get("lineage_policy") != corpus.hybrid.LINEAGE_POLICY
            or not isinstance(row.get("license"), str)
            or not row["license"]
            or row.get("source_license") != row.get("license")
            or not isinstance(row.get("licenses"), list)
            or row.get("license") not in row["licenses"]
        ):
            raise BenchmarkError(f"truth row {line_no} has invalid lineage/license fields")
        expected_selection_hash = corpus._selection_hash(row, str(region_id))
        if (
            not re_full_sha(row.get("selection_sha256"))
            or row.get("selection_sha256") != expected_selection_hash
        ):
            raise BenchmarkError(f"truth row {line_no} selection SHA-256 is not canonical")
        if family == corpus.hybrid.OVERTURE_FAMILY:
            overture_source = source_catalog[corpus.hybrid.OVERTURE_SOURCE_ID]
            retained = overture_source["retained_input"]
            acquisition = overture_source["acquisition_manifest"]
            provenance = row.get("coordinate_provenance")
            try:
                source_url = corpus.hybrid._safe_source_url(
                    row.get("source_url"), f"truth row {line_no} Overture source URL"
                )
            except corpus.CorpusError as exc:
                raise BenchmarkError(str(exc)) from exc
            if (
                region_id != corpus.COUNTRYWIDE_REGION_IDS[country]
                or role != "countrywide"
                or row.get("truth_source_id") != corpus.hybrid.OVERTURE_SOURCE_ID
                or not str(row.get("record_id", "")).startswith("overture:place:")
                or row.get("source_record_id")
                != str(row.get("record_id", "")).removeprefix("overture:place:")
                or row.get("source_theme") != overture_source["theme"]
                or row.get("source_type") != overture_source["type"]
                or row.get("source_sha256") != retained["sha256"]
                or row.get("source_artifact") != retained
                or row.get("source_release") != overture_source["source_release"]
                or row.get("source_snapshot_at") != overture_source["snapshot_at"]
                or not isinstance(provenance, Mapping)
                or provenance.get("source_family") != corpus.hybrid.OVERTURE_FAMILY
                or provenance.get("snapshot_sha256") != retained["sha256"]
                or provenance.get("snapshot_logical_name") != retained["logical_name"]
                or provenance.get("acquisition_manifest_sha256") != acquisition["sha256"]
                or provenance.get("source_snapshot_at") != overture_source["snapshot_at"]
                or provenance.get("same_export_as_indexed_sheet") is not False
                or provenance.get("source_url") != source_url
                or provenance.get("license") != row.get("license")
            ):
                raise BenchmarkError(f"truth row {line_no} mislabels an Overture row as regional")
        elif family == corpus.hybrid.OSM_FAMILY:
            region = corpus.REGION_CATALOG.get(region_id)
            if region is None:
                raise BenchmarkError(f"truth row {line_no} names an unknown direct OSM region")
            source = source_catalog.get(region["source_id"])
            if not isinstance(source, dict):
                raise BenchmarkError(f"truth row {line_no} names an unavailable OSM source")
            try:
                corpus._validate_osm_row_source(row, region, source, f"truth row {line_no}")
            except corpus.CorpusError as exc:
                raise BenchmarkError(str(exc)) from exc
        else:
            raise BenchmarkError(f"truth row {line_no} has an unsupported source family")
        source = source_catalog.get(row.get("truth_source_id"))
        retained = source.get("retained_input") if isinstance(source, dict) else None
        if not isinstance(retained, dict) or row.get("source_sha256") != retained.get("sha256"):
            raise BenchmarkError(f"truth row {line_no} disagrees with its source pin")
        record_id, query, coordinate = corpus.hybrid._dedupe_key(row)
        if record_id in seen_ids or query in seen_queries or coordinate in seen_coordinates:
            raise BenchmarkError(f"truth row {line_no} violates global deduplication")
        seen_ids.add(record_id)
        seen_queries.add(query)
        seen_coordinates.add(coordinate)
        by_region[str(region_id)] += 1
        by_country[str(country)] += 1
        if row.get("lineage_class") == "outside_chain":
            outside_by_country[str(country)] += 1
    if dict(sorted(by_region.items())) != expected_counts:
        raise BenchmarkError("truth rows do not realize the fixed sampling-region quotas")
    if dict(by_country) != {country: 300 for country in corpus.COUNTRIES}:
        raise BenchmarkError("truth rows do not contain exactly 300 rows per country")
    for country, minimum in corpus.OUTSIDE_MINIMA.items():
        if outside_by_country[country] < minimum:
            raise BenchmarkError(f"{country} outside_chain minimum is not met")
    matrices = corpus._count_matrices(rows, tuple(source_catalog))
    for field, expected in matrices.items():
        if manifest.get(field) != expected:
            raise BenchmarkError(f"truth manifest {field} matrix disagrees with rows")
    corpus.hybrid._verify_capture(truth_capture, "schema-5 truth corpus")
    corpus.hybrid._verify_capture(manifest_capture, "schema-5 truth manifest")
    return rows, manifest


def re_full_sha(value: object) -> bool:
    return isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value)


def _score_slice(pairs: Sequence[tuple[dict, tuple[float, float] | None]], threshold_m: float) -> dict:
    hits = 0
    misses = 0
    distances: list[float] = []
    for row, answer in pairs:
        if answer is None:
            misses += 1
            continue
        distance = public.haversine_m(float(row["lat"]), float(row["lon"]), answer[0], answer[1])
        distances.append(distance)
        if distance <= threshold_m:
            hits += 1
        else:
            misses += 1
    cases = len(pairs)
    return {
        "cases": cases,
        "hits": hits,
        "misses": misses,
        "hit_rate": hits / cases if cases else None,
        "maximum_distance_m": threshold_m,
        "empty_or_far_is_miss": True,
        "median_answer_distance_m": sorted(distances)[len(distances) // 2] if distances else None,
    }


def score_answers(
    truth: Sequence[dict],
    answers: Sequence[tuple[float, float] | None],
    threshold_m: float = MAX_DISTANCE_M,
) -> dict:
    if len(truth) != len(answers):
        raise BenchmarkError("answer count does not match truth count")
    if not math.isfinite(threshold_m) or threshold_m <= 0 or threshold_m > MAX_DISTANCE_M:
        raise BenchmarkError("distance threshold must be finite, positive, and <= 300 m")
    pairs = list(zip(truth, answers))
    countries: dict[str, dict] = {}
    direct_regions: dict[str, dict[str, dict]] = {}
    overture_countrywide: dict[str, dict] = {}
    region_spread: dict[str, dict] = {}
    for country in corpus.COUNTRIES:
        country_pairs = [pair for pair in pairs if pair[0]["country"] == country]
        countries[country] = _score_slice(country_pairs, threshold_m)
        overture_pairs = [
            pair for pair in country_pairs
            if pair[0]["truth_source_family"] == corpus.hybrid.OVERTURE_FAMILY
        ]
        overture_countrywide[country] = _score_slice(overture_pairs, threshold_m)
        direct_regions[country] = {}
        for region in sorted(
            (value for value in corpus.REGION_CATALOG.values() if value["country"] == country),
            key=lambda value: int(value["selection_order"]),
        ):
            region_pairs = [
                pair for pair in country_pairs
                if pair[0]["truth_source_family"] == corpus.hybrid.OSM_FAMILY
                and pair[0]["sampling_region_id"] == region["region_id"]
            ]
            direct_regions[country][str(region["region_id"])] = {
                "role": region["role"],
                **_score_slice(region_pairs, threshold_m),
            }
        rates = {
            region_id: score["hit_rate"]
            for region_id, score in direct_regions[country].items()
            if score["hit_rate"] is not None
        }
        best = max(rates, key=rates.get) if rates else None
        worst = min(rates, key=rates.get) if rates else None
        region_spread[country] = {
            "best_region_id": best,
            "worst_region_id": worst,
            "maximum_hit_rate": rates.get(best) if best else None,
            "minimum_hit_rate": rates.get(worst) if worst else None,
            "percentage_point_range": (
                (rates[best] - rates[worst]) * 100 if best and worst else None
            ),
            "direct_osm_only": True,
        }
    overture_pairs = [
        pair for pair in pairs
        if pair[0]["truth_source_family"] == corpus.hybrid.OVERTURE_FAMILY
    ]
    direct_pairs = [
        pair for pair in pairs
        if pair[0]["truth_source_family"] == corpus.hybrid.OSM_FAMILY
    ]
    return {
        "overall_descriptive": _score_slice(pairs, threshold_m),
        "countries_descriptive": countries,
        "direct_osm_overall": _score_slice(direct_pairs, threshold_m),
        "direct_osm_regions": direct_regions,
        "direct_osm_region_spread": region_spread,
        "overture_countrywide_overall": _score_slice(overture_pairs, threshold_m),
        "overture_countrywide_by_country": overture_countrywide,
        "interpretation": "descriptive_only",
        "headline_eligible": False,
    }


def command_validate(args: argparse.Namespace) -> dict:
    rows, manifest = validate_truth(args.truth)
    return {
        "schema": manifest["schema"],
        "rows": len(rows),
        "sha256": manifest["sha256"],
        "sampling_regions": len(manifest["rows_by_sampling_region"]),
        "interpretation": "descriptive_only",
        "headline_eligible": False,
    }


def command_run(args: argparse.Namespace) -> dict:
    if not math.isfinite(args.distance_m) or args.distance_m <= 0 or args.distance_m > MAX_DISTANCE_M:
        raise BenchmarkError("--distance-m must be finite, positive, and <= 300")
    truth_path = args.truth.expanduser().resolve()
    rows, manifest = validate_truth(truth_path)
    sheets = public._parse_sheet(args.sheet)
    truth_capture = public._capture_file(truth_path, "schema-5 truth corpus")
    manifest_capture = public._capture_file(_manifest_path(truth_path), "schema-5 truth manifest")
    runner_capture = public._capture_file(pathlib.Path(__file__), "schema-5 benchmark runner")
    binary_capture = public._capture_file(args.gridpin_bin, "GridPin binary")
    sheet_captures = {
        country: public._capture_file(path, f"GridPin {country} sheet")
        for country, path in sheets.items()
    }
    captures = [truth_capture, manifest_capture, runner_capture, binary_capture, *sheet_captures.values()]
    public._verify_captures(captures)
    overture_source = manifest["source_catalog"][corpus.hybrid.OVERTURE_SOURCE_ID]
    answers, artifacts = public._run_gridpin(
        rows,
        binary_capture["_path"],
        sheets,
        args.work,
        corpus_hash=truth_capture["sha256"],
        binary_capture=binary_capture,
        sheet_captures=sheet_captures,
        truth_source_details={
            "dataset": overture_source["dataset"],
            "theme": overture_source["theme"],
            "type": overture_source["type"],
            "source_release": overture_source["source_release"],
            "uri": overture_source["public_uri"],
        },
    )
    overture_separation = artifacts.get("source_separation")
    artifacts["source_separation"] = {
        "schema": 2,
        "scope": "per_truth_source_family",
        "overture_places": overture_separation,
        "openstreetmap": {
            "decision": "unknown_not_inferred_from_overture_preflight",
            "same_dataset_family": "not_evaluated",
            "reason": (
                "direct OSM truth is scored separately by region; sheet/source "
                "relationships require per-sheet evidence and are not inferred"
            ),
        },
        "mixed_corpus_decision": "not_asserted",
    }
    result = {
        "schema": RESULT_SCHEMA,
        "generated_at": public._utc_now(),
        "metric": {"name": "hit@1", "maximum_distance_m": args.distance_m},
        "runner": public._public_capture(runner_capture),
        "truth": {
            "sha256": truth_capture["sha256"],
            "manifest_sha256": manifest_capture["sha256"],
            "rows": len(rows),
            "rows_by_country": manifest["rows_by_country"],
            "rows_by_sampling_region": manifest["rows_by_sampling_region"],
            "rows_by_country_and_sampling_region": manifest["rows_by_country_and_sampling_region"],
            "region_catalog": manifest["region_catalog"],
            "source_catalog": manifest["source_catalog"],
            "fragment_manifests": manifest["fragment_manifests"],
        },
        "gridpin_artifacts": public._public_gridpin_artifacts(artifacts),
        "results": {"gridpin": score_answers(rows, answers, args.distance_m)},
        "comparison_design": {
            "mixed_source_overall": "descriptive_only",
            "regional_scores": "direct_osm_only",
            "overture_scores": "countrywide_separate",
            "headline_eligible": False,
        },
        "interpretation": "descriptive_only",
        "headline_eligible": False,
    }
    public._verify_captures(captures)
    public._atomic_json_noreplace(args.output, public._sanitize_json_paths(result))
    return result


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    validate = commands.add_parser("validate", help="validate schema-5 corpus/manifest coupling")
    validate.add_argument("--truth", required=True, type=pathlib.Path)
    validate.set_defaults(handler=command_validate)
    run = commands.add_parser("run", help="run local GridPin batches and score fixed slices")
    run.add_argument("--truth", required=True, type=pathlib.Path)
    run.add_argument("--gridpin-bin", required=True, type=pathlib.Path)
    run.add_argument("--sheet", action="append", default=[], metavar="CC=PATH", required=True)
    run.add_argument("--work", required=True, type=pathlib.Path)
    run.add_argument("--output", required=True, type=pathlib.Path)
    run.add_argument("--distance-m", type=float, default=MAX_DISTANCE_M)
    run.set_defaults(handler=command_run)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    result = args.handler(args)
    print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
