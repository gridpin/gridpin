#!/usr/bin/env python3
"""Add strict BNetzA source-only address points to the accepted DE builder.

Locality evidence deliberately comes from the immutable pre-BNetzA builder.
The accepted postcode-fill builder is used only as the output base.  This
separation prevents a BNetzA postcode fill from validating another BNetzA
source-only address point.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
from collections.abc import Mapping, Sequence
import csv
from dataclasses import dataclass
import itertools
import json
import math
import os
import pathlib
import shutil
from typing import Any

import de_bnetza_address_supplement as bnetza
import de_photon_osm_supplement as builder


SCHEMA = "gridpin-de-bnetza-exact-city-postcode-supplement-v1"
LOCALITY_MODE = "exact_city_postcode"
STATUS = "PUBLIC_PERMISSIVE_DEVELOPMENT"
EXPECTED_REAL_SOURCE_ROWS_ADDED = 3_161
EXPECTED_REAL_OUTPUT_POSTCODE_FILLS = 14_528
EXPECTED_REAL_BUILDER_ROWS = 19_267_049
EXPECTED_REAL_CANDIDATE_GROUPS = 49_360

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
RAW_SOURCE = REPO_ROOT / (
    "code/eval/work/de_f5_wave_c_bnetza_20260821/"
    "Ladesaeulenregister_BNetzA_2026-07-28.csv"
)
RAW_PIN = bnetza.Pin(
    "18e10299d7af901854a043b03595263dd2beb67cb9d66a8a0be0101ce47780d5",
    54_596_908,
)
TERMS_SNAPSHOT = REPO_ROOT / (
    "code/eval/work/de_continuous_product_optimization_v1/"
    "bnetza_ladesaeulenkarte_terms_20260829.html"
)
TERMS_PIN = bnetza.Pin(
    "ed0f8d0d976365de876a6d2e2deb16a90daa3b2a81accaff93677b83d500f399",
    65_626,
)
CANDIDATE_FRAME = REPO_ROOT / (
    "code/eval/work/de_f5_wave_c_bnetza_20260821/candidate_frame_v1.jsonl"
)
CANDIDATE_FRAME_PIN = bnetza.Pin(
    "11d8485e78e40cd173e40ea42b0942583bd54985590f27791517f0cde4104c3c",
    227_731_750,
)
LOCALITY_CONTEXT_BUILDER = REPO_ROOT / "code/data/build_de.csv.gz"
LOCALITY_CONTEXT_BUILDER_PIN = bnetza.Pin(
    "f111c3f6ccd498a3237d3d01602c8fafc949d31253e1e5a07c79de9443db4098",
    247_594_238,
)
OUTPUT_BUILDER = REPO_ROOT / (
    "code/eval/work/de_continuous_product_optimization_v1/"
    "bnetza_postcode_fill_only_v1/"
    "build_de_permissive_bnetza_postcode_fill_only_v1.csv.gz"
)
OUTPUT_BUILDER_PIN = bnetza.Pin(
    "a40543295cfd261a2e76012b45d7d3712b2987abc09fa9ef6cbb1e4fb599f116",
    248_839_581,
)
SOURCE_URL = (
    "https://data.bundesnetzagentur.de/Bundesnetzagentur/DE/Fachthemen/"
    "ElektrizitaetundGas/E-Mobilitaet/"
    "Ladesaeulenregister_BNetzA_2026-07-28.csv"
)
TERMS_URL = (
    "https://www.bundesnetzagentur.de/DE/Fachthemen/ElektrizitaetundGas/"
    "E-Mobilitaet/Ladesaeulenkarte/start.html"
)


class ExactCityPostcodeSupplementError(RuntimeError):
    """The strict source-only supplement could not be proven safe."""


@dataclass(frozen=True)
class Config:
    source: bnetza.SourceContract
    candidate_frame: pathlib.Path
    candidate_frame_pin: bnetza.Pin
    locality_context_builder: pathlib.Path
    locality_context_builder_pin: bnetza.Pin
    output_builder: pathlib.Path
    output_builder_pin: bnetza.Pin
    expected_builder_rows: int
    expected_candidate_groups: int
    expected_source_rows_added: int
    expected_output_postcode_fills: int
    output_csv: pathlib.Path | None
    receipt: pathlib.Path | None
    minimum_free_bytes: int = bnetza.DEFAULT_MIN_FREE_BYTES


def frozen_source_contract() -> bnetza.SourceContract:
    return bnetza.SourceContract(
        raw_source=RAW_SOURCE,
        raw_pin=RAW_PIN,
        terms_snapshot=TERMS_SNAPSHOT,
        terms_pin=TERMS_PIN,
        source_url=SOURCE_URL,
        terms_url=TERMS_URL,
        snapshot_date="2026-07-28",
        embedded_update="Letzte Aktualisierung vom: 28.07.2026",
    )


def frozen_config(
    *,
    output_csv: pathlib.Path | None = None,
    receipt: pathlib.Path | None = None,
    minimum_free_bytes: int = bnetza.DEFAULT_MIN_FREE_BYTES,
) -> Config:
    return Config(
        source=frozen_source_contract(),
        candidate_frame=CANDIDATE_FRAME,
        candidate_frame_pin=CANDIDATE_FRAME_PIN,
        locality_context_builder=LOCALITY_CONTEXT_BUILDER,
        locality_context_builder_pin=LOCALITY_CONTEXT_BUILDER_PIN,
        output_builder=OUTPUT_BUILDER,
        output_builder_pin=OUTPUT_BUILDER_PIN,
        expected_builder_rows=EXPECTED_REAL_BUILDER_ROWS,
        expected_candidate_groups=EXPECTED_REAL_CANDIDATE_GROUPS,
        expected_source_rows_added=EXPECTED_REAL_SOURCE_ROWS_ADDED,
        expected_output_postcode_fills=EXPECTED_REAL_OUTPUT_POSTCODE_FILLS,
        output_csv=output_csv,
        receipt=receipt,
        minimum_free_bytes=minimum_free_bytes,
    )


def _raise(message: str) -> None:
    raise ExactCityPostcodeSupplementError(message)


def _validate_config(config: Config, *, for_build: bool) -> None:
    for label, pin in (
        ("raw source", config.source.raw_pin),
        ("terms snapshot", config.source.terms_pin),
        ("candidate frame", config.candidate_frame_pin),
        ("locality-context builder", config.locality_context_builder_pin),
        ("output builder", config.output_builder_pin),
    ):
        try:
            bnetza._validate_pin(pin, label)
        except bnetza.BNetzASupplementError as exc:
            raise ExactCityPostcodeSupplementError(str(exc)) from exc
    if config.source.license != bnetza.LICENSE:
        _raise("BNetzA source license drift")
    if config.source.attribution != bnetza.ATTRIBUTION:
        _raise("BNetzA source attribution drift")
    for label, count in (
        ("builder rows", config.expected_builder_rows),
        ("candidate groups", config.expected_candidate_groups),
        ("source rows added", config.expected_source_rows_added),
    ):
        if isinstance(count, bool) or count <= 0:
            _raise(f"expected {label} must be positive")
    if config.expected_output_postcode_fills < 0:
        _raise("expected output postcode fills must be non-negative")
    if config.minimum_free_bytes < 0:
        _raise("minimum free bytes must be non-negative")
    if for_build:
        if config.output_csv is None or config.receipt is None:
            _raise("build requires output CSV and receipt paths")
        for path in (config.output_csv, config.receipt):
            if path.exists():
                _raise(f"output must not exist: {path}")
            if not path.parent.is_dir():
                _raise(f"output parent is missing: {path.parent}")


def _open(path: pathlib.Path, pin: bnetza.Pin, label: str) -> builder.PinnedFile:
    try:
        return bnetza._open_pinned(path, pin, label)
    except bnetza.BNetzASupplementError as exc:
        raise ExactCityPostcodeSupplementError(str(exc)) from exc


def _recheck(pinned: builder.PinnedFile, label: str) -> None:
    try:
        bnetza._recheck_pinned(pinned, label)
    except bnetza.BNetzASupplementError as exc:
        raise ExactCityPostcodeSupplementError(str(exc)) from exc


def _resolved_row(
    projection: bnetza.Projection,
    locality: bnetza.Locality,
) -> bnetza.ResolvedProjection:
    coordinate = bnetza._medoid(projection.coordinates)
    row = {
        "nom_voie_norm": projection.street_norm,
        "code_insee": locality.code,
        "nom_commune_norm": locality.norm,
        "code_postal": projection.postcode,
        "code_postal_display": projection.postcode,
        "numero": str(projection.number),
        "rep": projection.suffix,
        "lon": format(coordinate.lon, ".15g"),
        "lat": format(coordinate.lat, ".15g"),
        "nom_voie": projection.street_display,
        "nom_commune": locality.display,
        "provincia_norm": locality.province or projection.state,
    }
    return bnetza.ResolvedProjection(
        identity=bnetza._row_identity(row),
        row=row,
        source=projection,
        locality_mode=LOCALITY_MODE,
    )


def resolve_exact_city_postcode_source_only(
    projections: Sequence[bnetza.Projection],
    context: bnetza.BaseContext,
) -> tuple[tuple[bnetza.ResolvedProjection, ...], dict[str, int]]:
    """Resolve only a unique exact city+postcode relation from original base data."""

    counts: defaultdict[str, int] = defaultdict(int)
    grouped: dict[
        tuple[str, str, int, str], list[bnetza.ResolvedProjection]
    ] = defaultdict(list)
    for projection in projections:
        counts["source_projections_seen"] += 1
        observation = context.address_observations.get(projection.semantic_key)
        if observation is not None:
            if observation.postcodes - {projection.postcode}:
                counts["excluded_conflicting_exact_address_postcode"] += 1
            elif len(observation.codes) != 1:
                counts["excluded_ambiguous_exact_address_locality"] += 1
            else:
                counts["excluded_existing_base_identity"] += 1
            continue

        city = [
            value
            for value in context.localities.values()
            if value.norm == projection.city_norm
        ]
        compatible = [
            value
            for value in city
            if not value.province or value.province == projection.state
        ]
        postcode = [
            value for value in compatible if projection.postcode in value.postcodes
        ]
        if len(postcode) > 1:
            counts["excluded_ambiguous_exact_city_postcode"] += 1
            continue
        if len(postcode) == 1:
            resolved_value = _resolved_row(projection, postcode[0])
            grouped[resolved_value.identity].append(resolved_value)
            counts["candidate_exact_city_postcode"] += 1
            continue

        state_known = [
            value for value in compatible if value.province == projection.state
        ]
        if len(state_known) > 1:
            counts["excluded_ambiguous_exact_city_state"] += 1
        elif len(state_known) == 1:
            counts["excluded_other_locality_mode_exact_city_state"] += 1
        elif len(city) == 1:
            counts["excluded_other_locality_mode_unique_city"] += 1
        elif compatible:
            counts["excluded_ambiguous_city"] += 1
        else:
            counts["excluded_other_locality_mode_hashed_new_city"] += 1

    resolved: list[bnetza.ResolvedProjection] = []
    for identity in sorted(grouped):
        values = grouped[identity]
        signatures = {
            (
                value.row["nom_commune_norm"],
                value.row["code_postal_display"],
                value.source.state_code,
            )
            for value in values
        }
        coordinates = tuple(
            coordinate for value in values for coordinate in value.source.coordinates
        )
        if (
            len(values) != 1
            or len(signatures) != 1
            or bnetza._coordinate_spread(coordinates) > bnetza.MAX_COORDINATE_SPREAD_M
        ):
            counts["excluded_conflicting_resolved_projection"] += len(values)
            continue
        value = values[0]
        if value.source.source_groups > 1:
            counts["admitted_identical_source_groups_collapsed"] += (
                value.source.source_groups - 1
            )
        resolved.append(value)

    for name in (
        "excluded_existing_base_identity",
        "candidate_exact_city_postcode",
        "excluded_other_locality_mode_exact_city_state",
        "excluded_other_locality_mode_unique_city",
        "excluded_other_locality_mode_hashed_new_city",
        "excluded_conflicting_exact_address_postcode",
        "excluded_ambiguous_exact_address_locality",
        "excluded_ambiguous_exact_city_postcode",
        "excluded_ambiguous_exact_city_state",
        "excluded_ambiguous_city",
        "excluded_conflicting_resolved_projection",
        "admitted_identical_source_groups_collapsed",
    ):
        counts[name] += 0
    counts["source_only_exact_city_postcode_admitted"] = len(resolved)
    counts["unique_semantic_output_rows"] = len(resolved)
    if not resolved:
        _raise("strict exact-city+postcode source-only selection is vacuous")
    return tuple(resolved), dict(sorted(counts.items()))


def validate_builder_relationship(
    locality_context: builder.PinnedFile,
    output_base: builder.PinnedFile,
    *,
    expected_rows: int,
) -> dict[str, int]:
    """Prove that output base changes only blank postcode metadata, row for row."""

    left_text, left_reader = bnetza._csv_reader(locality_context)
    right_text, right_reader = bnetza._csv_reader(output_base)
    counts: defaultdict[str, int] = defaultdict(int)
    unchanged_fields = tuple(
        field
        for field in builder.BUILDER_HEADER
        if field not in {"code_postal", "code_postal_display"}
    )
    left_previous: tuple[Any, ...] | None = None
    right_previous: tuple[Any, ...] | None = None

    def raw_postcode(row: Mapping[str, str], *, label: str, row_number: int) -> str:
        numeric = row["code_postal"].strip()
        display = row["code_postal_display"].strip()
        if (numeric, display) == ("", ""):
            return ""
        if numeric == display and bnetza._POSTCODE.fullmatch(numeric) is not None:
            return numeric
        _raise(f"{label} builder has malformed raw postcode pair at row {row_number}")

    try:
        pairs = itertools.zip_longest(left_reader, right_reader)
        for row_number, pair in enumerate(pairs, 1):
            before, after = pair
            if before is None or after is None:
                _raise("locality-context/output builders have different row counts")
            counts["builder_rows_compared"] += 1
            left_key = bnetza._row_sort_key(before)
            right_key = bnetza._row_sort_key(after)
            if left_previous is not None and left_key < left_previous:
                _raise("locality-context builder is not canonically sorted")
            if right_previous is not None and right_key < right_previous:
                _raise("output builder is not canonically sorted")
            left_previous, right_previous = left_key, right_key
            if any(before[field] != after[field] for field in unchanged_fields):
                _raise(f"output builder changed a non-postcode field at row {row_number}")
            before_postcode = raw_postcode(
                before,
                label="locality-context",
                row_number=row_number,
            )
            after_postcode = raw_postcode(
                after,
                label="output",
                row_number=row_number,
            )
            if before_postcode:
                if after_postcode != before_postcode:
                    _raise(f"output builder changed a nonblank postcode at row {row_number}")
                counts["builder_rows_unchanged_nonblank_postcode"] += 1
            elif after_postcode:
                if (
                    after["code_postal"] != after_postcode
                    or after["code_postal_display"] != after_postcode
                ):
                    _raise(f"output builder has conflicting filled postcode fields at row {row_number}")
                counts["output_builder_blank_postcodes_filled"] += 1
            else:
                counts["builder_rows_unchanged_blank_postcode"] += 1
    finally:
        left_text.close()
        right_text.close()
    if counts["builder_rows_compared"] != expected_rows:
        _raise(
            "builder row-count pin mismatch: "
            f"got {counts['builder_rows_compared']}, expected {expected_rows}"
        )
    counts["base_rows_order_and_coordinates_preserved"] = expected_rows
    return dict(sorted(counts.items()))


def _audit_internal(
    config: Config,
) -> tuple[tuple[bnetza.ResolvedProjection, ...], dict[str, Any]]:
    _validate_config(config, for_build=False)
    raw = _open(config.source.raw_source, config.source.raw_pin, "raw BNetzA source")
    terms: builder.PinnedFile | None = None
    frame: builder.PinnedFile | None = None
    locality_context: builder.PinnedFile | None = None
    output_base: builder.PinnedFile | None = None
    try:
        terms = _open(
            config.source.terms_snapshot,
            config.source.terms_pin,
            "BNetzA terms snapshot",
        )
        frame = _open(
            config.candidate_frame,
            config.candidate_frame_pin,
            "BNetzA candidate frame",
        )
        locality_context = _open(
            config.locality_context_builder,
            config.locality_context_builder_pin,
            "immutable locality-context builder",
        )
        output_base = _open(
            config.output_builder,
            config.output_builder_pin,
            "accepted output builder",
        )
        try:
            projections, source_counts, metadata = bnetza.load_projections(
                frame,
                expected_candidate_groups=config.expected_candidate_groups,
            )
            bnetza._validate_source_contract(
                metadata,
                bnetza.Config(
                    source=config.source,
                    candidate_frame=config.candidate_frame,
                    candidate_frame_pin=config.candidate_frame_pin,
                    builder_csv=config.locality_context_builder,
                    builder_pin=config.locality_context_builder_pin,
                    expected_builder_rows=config.expected_builder_rows,
                    expected_candidate_groups=config.expected_candidate_groups,
                    output_csv=pathlib.Path("unused.csv.gz"),
                    receipt=pathlib.Path("unused.receipt.json"),
                    status=STATUS,
                    overlay_mode=bnetza.OVERLAY_ADD_AND_FILL,
                    minimum_free_bytes=0,
                ),
            )
            context = bnetza.load_base_context(
                locality_context,
                projections,
                expected_rows=config.expected_builder_rows,
            )
            resolved, resolution_counts = resolve_exact_city_postcode_source_only(
                projections,
                context,
            )
            relationship_counts = validate_builder_relationship(
                locality_context,
                output_base,
                expected_rows=config.expected_builder_rows,
            )
        except bnetza.BNetzASupplementError as exc:
            raise ExactCityPostcodeSupplementError(str(exc)) from exc
        if len(resolved) != config.expected_source_rows_added:
            _raise(
                "strict source-row count mismatch: "
                f"got {len(resolved)}, expected {config.expected_source_rows_added}"
            )
        if (
            relationship_counts.get("output_builder_blank_postcodes_filled", 0)
            != config.expected_output_postcode_fills
        ):
            _raise(
                "accepted output-builder fill count mismatch: got "
                f"{relationship_counts.get('output_builder_blank_postcodes_filled', 0)}, "
                f"expected {config.expected_output_postcode_fills}"
            )
        for label, pinned in (
            ("raw BNetzA source", raw),
            ("BNetzA terms snapshot", terms),
            ("BNetzA candidate frame", frame),
            ("immutable locality-context builder", locality_context),
            ("accepted output builder", output_base),
        ):
            _recheck(pinned, label)
        counts = dict(
            sorted({**source_counts, **resolution_counts, **relationship_counts}.items())
        )
        report: dict[str, Any] = {
            "schema": SCHEMA,
            "status": STATUS,
            "policy": {
                "locality_context_is_immutable_pre_bnetza_builder": True,
                "self_confirming_postcode_cascade": False,
                "source_only_address_points": True,
                "admitted_locality_mode": LOCALITY_MODE,
                "all_other_locality_modes_fail_closed": True,
                "base_rows_order_and_coordinates_preserved": True,
                "source_coordinates_only_for_unique_semantic_output_rows": True,
                "identical_physical_source_groups_require_coordinate_consensus": True,
                "identical_source_group_consensus_bound_m": bnetza.MAX_COORDINATE_SPREAD_M,
                "conflicting_projection_fail_closed": True,
                "first_candidate_selection": False,
                "network_calls": 0,
                "gridpin_engine_calls": 0,
                "photon_engine_calls": 0,
            },
            "configuration": {
                "expected_builder_rows": config.expected_builder_rows,
                "expected_candidate_groups": config.expected_candidate_groups,
                "expected_source_rows_added": config.expected_source_rows_added,
                "expected_output_postcode_fills": config.expected_output_postcode_fills,
            },
            "license": {
                "data": config.source.license,
                "attribution": config.source.attribution,
                "license_url": config.source.terms_url,
                "raw_source_url": config.source.source_url,
                "snapshot_date": config.source.snapshot_date,
            },
            "inputs": {
                "raw_source": dict(raw.evidence),
                "terms_snapshot": dict(terms.evidence),
                "candidate_frame": dict(frame.evidence),
                "locality_context_builder": dict(locality_context.evidence),
                "output_builder": dict(output_base.evidence),
            },
            "counts": counts,
        }
        return resolved, report
    finally:
        raw.stream.close()
        for pinned in (terms, frame, locality_context, output_base):
            if pinned is not None:
                pinned.stream.close()


def audit(config: Config) -> dict[str, Any]:
    """Run the full pinned accounting without creating an output builder."""

    _, report = _audit_internal(config)
    return report


def build(config: Config) -> dict[str, Any]:
    """Build the write-once strict source-only supplement and receipt."""

    _validate_config(config, for_build=True)
    assert config.output_csv is not None
    assert config.receipt is not None
    free = shutil.disk_usage(config.output_csv.parent).free
    if free < config.minimum_free_bytes:
        _raise(f"disk floor crossed before build: {free} < {config.minimum_free_bytes}")
    resolved, report = _audit_internal(config)
    output_base = _open(
        config.output_builder,
        config.output_builder_pin,
        "accepted output builder",
    )
    try:
        with builder._canonical_gzip_writer(
            config.output_csv,
            builder.BUILDER_HEADER,
        ) as writer:
            try:
                merge_counts = bnetza.merge_output(
                    output_base,
                    resolved,
                    writer,
                    overlay_mode=bnetza.OVERLAY_ADD_AND_FILL,
                )
            except bnetza.BNetzASupplementError as exc:
                raise ExactCityPostcodeSupplementError(str(exc)) from exc
        _recheck(output_base, "accepted output builder")
    finally:
        output_base.stream.close()
    forbidden_merge_counts = (
        "base_blank_postcode_rows_filled",
        "source_rows_consumed_by_base_identity",
        "source_rows_already_present",
        "source_rows_quarantined_postcode_conflict",
        "source_only_rows_skipped_by_policy",
    )
    if any(merge_counts.get(name, 0) for name in forbidden_merge_counts):
        _raise("source-only merge touched or intersected an accepted base identity")
    if merge_counts.get("base_rows_retained", 0) != config.expected_builder_rows:
        _raise("source-only merge did not retain every accepted base row")
    if merge_counts.get("source_rows_added", 0) != config.expected_source_rows_added:
        _raise("source-only merge addition count drift")
    if merge_counts.get("output_rows", 0) != (
        config.expected_builder_rows + config.expected_source_rows_added
    ):
        _raise("source-only merge output row conservation failed")
    report["counts"] = dict(sorted({**report["counts"], **merge_counts}.items()))
    report["output"] = bnetza._file_evidence(config.output_csv)
    with config.receipt.open("xb") as handle:
        handle.write(bnetza.canonical_json_bytes(report))
        handle.flush()
        os.fsync(handle.fileno())
    return report


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audit-only", action="store_true")
    parser.add_argument("--output-csv", type=pathlib.Path)
    parser.add_argument("--receipt", type=pathlib.Path)
    parser.add_argument(
        "--minimum-free-bytes",
        type=int,
        default=bnetza.DEFAULT_MIN_FREE_BYTES,
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    if args.audit_only and (args.output_csv is not None or args.receipt is not None):
        raise SystemExit("--audit-only does not accept output paths")
    if not args.audit_only and (args.output_csv is None or args.receipt is None):
        raise SystemExit("build requires --output-csv and --receipt")
    config = frozen_config(
        output_csv=args.output_csv,
        receipt=args.receipt,
        minimum_free_bytes=args.minimum_free_bytes,
    )
    try:
        result = audit(config) if args.audit_only else build(config)
    except ExactCityPostcodeSupplementError as exc:
        raise SystemExit(f"DE BNetzA strict source-only supplement refused: {exc}") from exc
    print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
