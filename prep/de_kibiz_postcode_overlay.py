#!/usr/bin/env python3
"""Fill blank DE builder postcodes from the frozen official NRW KiBiz roster.

This is a metadata-only, outcome-blind overlay.  It reads five safe address
fields from a byte-pinned post-prior roster and never admits a source point.
Only an exact street/locality/single-house/suffix identity already present in
the pinned builder can be changed, and every ambiguity is fail-closed.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
from collections.abc import Mapping, Sequence
import csv
from dataclasses import dataclass, field
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
from typing import Any, TextIO

import de_photon_osm_supplement as builder


SCHEMA = "gridpin-de-kibiz-postcode-overlay-v1"
STATUS = "PUBLIC_PERMISSIVE_DEVELOPMENT"
LICENSE = "DL-DE-BY-2.0"
LICENSE_NAME = "Datenlizenz Deutschland - Namensnennung - Version 2.0"
LICENSE_URL = "https://www.govdata.de/dl-de/by-2-0"
ATTRIBUTION = "Land NRW, Abrufdatum"
PUBLISHER = (
    "Ministerium fuer Kinder, Jugend, Familie, Gleichstellung, Flucht und "
    "Integration des Landes Nordrhein-Westfalen"
)
DATA_PROVIDER = "Land Nordrhein-Westfalen"
SOURCE_FAMILY = "official_nrw_kibiz_childcare_services"
SOURCE_KEY = "nrw_kibiz_childcare_services"
COLLECTION_ID = "governmentalservice"
PROVENANCE_SCHEMA = "gridpin-de-kibiz-wave-preregistration-v1"
AUDIT_SCHEMA = "gridpin-de-kibiz-full-population-source-audit-v1"
SOURCE_CARD_SCHEMA = "gridpin-de-kibiz-source-card-v1"
SAFE_ROSTER_FIELDS = (
    "street_name",
    "house_designator",
    "postal_code",
    "locality",
    "state_code",
)
DEFAULT_MIN_FREE_BYTES = 10 * 2**30
MAX_JSONL_LINE_BYTES = 1 * 2**20
MAX_MATCHED_IDENTITY_ROWS = 100_000
_POSTCODE = re.compile(r"[0-9]{5}")
_POSITIVE_SINGLE_HOUSE = re.compile(r"\s*([0-9]+)\s*([A-Za-z]?)\s*")
_SHA256 = re.compile(r"[0-9a-f]{64}")

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
FROZEN_ROSTER = (
    REPOSITORY_ROOT
    / "code/eval/work/de_kibiz_full_population_freeze_v1/post_prior_roster_v1.jsonl"
)
FROZEN_PROVENANCE = REPOSITORY_ROOT / "code/eval/de_kibiz_wave_preregistration_v1.json"
FROZEN_SOURCE_AUDIT = (
    REPOSITORY_ROOT
    / "code/eval/work/de_kibiz_full_population_freeze_v1/source_audit.json"
)
FROZEN_SOURCE_CARD = REPOSITORY_ROOT / "code/prep/de_kibiz_source_v1.json"
FROZEN_TERMS_SNAPSHOT = (
    REPOSITORY_ROOT
    / "code/eval/work/"
    "de_kibiz_metadata_witness_recapture_53b3ef8be1b71ed786445b1ff9ae110051b6fb30/"
    "metadata_05_iso_dataset.body"
)


class KiBizOverlayError(RuntimeError):
    """The overlay could not be produced without weakening a guard."""


@dataclass(frozen=True)
class Pin:
    sha256: str
    bytes: int


FROZEN_ROSTER_PIN = Pin(
    "dfb4da4fb41d321c361d1661973fef94d2be444e58040886daa3a36afaa15d7b",
    5_688_782,
)
FROZEN_PROVENANCE_PIN = Pin(
    "dcff894fe8e4b7f6a9cff7d4e1e9bc0262d28307eaca7dc819ae205161ca5199",
    35_635,
)
FROZEN_SOURCE_AUDIT_PIN = Pin(
    "86faf167a838bd7118cec4a3ccbfe121cf7923bbd8d7a1e3892fc2b08e366449",
    1_282,
)
FROZEN_SOURCE_CARD_PIN = Pin(
    "7a92f4b45566fa51211097e3b709e68f754db6b0c45891b171d103667f9403bd",
    2_084,
)
FROZEN_TERMS_SNAPSHOT_PIN = Pin(
    "6cc7307dda5846f42854c56b70de23b545d97587477ac1c9d8ca2e346cf1e0fe",
    30_079,
)


@dataclass(frozen=True)
class SourceContract:
    source_card: Path
    source_card_pin: Pin
    roster: Path
    roster_pin: Pin
    provenance: Path
    provenance_pin: Pin
    source_audit: Path
    source_audit_pin: Pin
    terms_snapshot: Path
    terms_snapshot_pin: Pin
    snapshot_date: str


@dataclass(frozen=True)
class Config:
    source: SourceContract
    expected_source_rows: int
    builder_csv: Path
    builder_pin: Pin
    expected_builder_rows: int
    output_csv: Path
    receipt: Path
    status: str = STATUS
    minimum_free_bytes: int = DEFAULT_MIN_FREE_BYTES


@dataclass(frozen=True)
class Projection:
    street_norm: str
    locality_norm: str
    number: int
    suffix: str
    postcode: str

    @property
    def identity(self) -> tuple[str, str, int, str]:
        return (self.street_norm, self.locality_norm, self.number, self.suffix)


@dataclass
class BaseObservation:
    codes: set[str] = field(default_factory=set)
    postcodes: set[str] = field(default_factory=set)
    rows: int = 0
    blank_rows: int = 0
    malformed_postcode: bool = False
    malformed_locality_code: bool = False


def canonical_json_bytes(value: Mapping[str, Any]) -> bytes:
    return (
        json.dumps(
            value,
            ensure_ascii=False,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    ).encode("utf-8")


def _validate_pin(pin: Pin, label: str) -> None:
    if _SHA256.fullmatch(pin.sha256) is None:
        raise KiBizOverlayError(f"{label} SHA-256 pin is invalid")
    if isinstance(pin.bytes, bool) or pin.bytes <= 0:
        raise KiBizOverlayError(f"{label} byte pin must be positive")


def _as_builder_pin(pin: Pin) -> builder.Pin:
    return builder.Pin(sha256=pin.sha256, bytes=pin.bytes)


def _open_pinned(path: Path, pin: Pin, label: str) -> builder.PinnedFile:
    _validate_pin(pin, label)
    try:
        return builder._open_pinned(path, _as_builder_pin(pin), label)
    except builder.SupplementError as exc:
        raise KiBizOverlayError(str(exc)) from exc


def _recheck_pinned(pinned: builder.PinnedFile, label: str) -> None:
    try:
        builder._recheck_pinned(pinned, label)
    except builder.SupplementError as exc:
        raise KiBizOverlayError(str(exc)) from exc


def _strict_json(raw: str, label: str) -> Any:
    try:
        return builder.strict_json_loads(raw)
    except (ValueError, json.JSONDecodeError) as exc:
        raise KiBizOverlayError(f"{label}: invalid strict JSON: {exc}") from exc


def _read_pinned_json(pinned: builder.PinnedFile, label: str) -> Mapping[str, Any]:
    pinned.stream.seek(0)
    payload = pinned.stream.read()
    try:
        raw = payload.decode("utf-8", errors="strict")
    except UnicodeDecodeError as exc:
        raise KiBizOverlayError(f"{label} is not strict UTF-8") from exc
    value = _strict_json(raw, label)
    if not isinstance(value, dict):
        raise KiBizOverlayError(f"{label} must be a JSON object")
    return value


def _validate_provenance(
    source_card: Mapping[str, Any],
    provenance: Mapping[str, Any],
    source_audit: Mapping[str, Any],
    *,
    contract: SourceContract,
    expected_source_rows: int,
) -> None:
    if provenance.get("schema") != PROVENANCE_SCHEMA:
        raise KiBizOverlayError("KiBiz provenance schema drift")
    source_doc = provenance.get("source")
    if not isinstance(source_doc, dict):
        raise KiBizOverlayError("KiBiz provenance source is missing")
    exact_source = {
        "source_key": SOURCE_KEY,
        "source_family": SOURCE_FAMILY,
        "publisher": PUBLISHER,
        "data_provider": DATA_PROVIDER,
        "collection_id": COLLECTION_ID,
    }
    for key, expected in exact_source.items():
        if source_doc.get(key) != expected:
            raise KiBizOverlayError(f"KiBiz provenance drift at source.{key}")
    license_doc = source_doc.get("license")
    if not isinstance(license_doc, dict):
        raise KiBizOverlayError("KiBiz license evidence is missing")
    exact_license = {
        "name": LICENSE_NAME,
        "identifier": LICENSE,
        "url": LICENSE_URL,
        "required_attribution": ATTRIBUTION,
        "changes_must_be_marked": True,
    }
    for key, expected in exact_license.items():
        if license_doc.get(key) != expected:
            raise KiBizOverlayError(f"KiBiz license drift at {key}")
    if source_audit.get("schema") != AUDIT_SCHEMA:
        raise KiBizOverlayError("KiBiz source-audit schema drift")
    audit_checks = {
        "outcome_blind_freeze": True,
        "post_prior_unique_count": expected_source_rows,
        "engine_calls_authorized": False,
        "engine_calls_performed": False,
        "competitor_calls_authorized": False,
        "competitor_calls_performed": False,
        "source_family_count": 1,
    }
    for key, expected in audit_checks.items():
        if source_audit.get(key) != expected:
            raise KiBizOverlayError(f"KiBiz source-audit drift at {key}")
    if source_card.get("schema") != SOURCE_CARD_SCHEMA:
        raise KiBizOverlayError("KiBiz source-card schema drift")
    card_license = source_card.get("license")
    if not isinstance(card_license, dict):
        raise KiBizOverlayError("KiBiz source-card license is missing")
    card_license_checks = {
        "name": LICENSE_NAME,
        "identifier": LICENSE,
        "url": LICENSE_URL,
        "changes_marked": True,
    }
    for key, expected in card_license_checks.items():
        if card_license.get(key) != expected:
            raise KiBizOverlayError(f"KiBiz source-card license drift at {key}")
    expected_attribution = (
        f"{ATTRIBUTION} {contract.snapshot_date}; modified postcode-metadata projection by GridPin"
    )
    if source_card.get("attribution") != expected_attribution:
        raise KiBizOverlayError("KiBiz source-card attribution drift")

    def expected_path(path: Path) -> str:
        resolved = path.resolve(strict=True)
        try:
            return str(resolved.relative_to(REPOSITORY_ROOT))
        except ValueError:
            return str(path)

    safe_roster = source_card.get("safe_roster")
    if not isinstance(safe_roster, dict) or safe_roster != {
        "path": expected_path(contract.roster),
        "bytes": contract.roster_pin.bytes,
        "sha256": contract.roster_pin.sha256,
        "records": expected_source_rows,
    }:
        raise KiBizOverlayError("KiBiz source-card roster binding drift")
    card_provenance = source_card.get("provenance")
    if not isinstance(card_provenance, dict):
        raise KiBizOverlayError("KiBiz source-card provenance is missing")
    expected_preregistration = {
        "path": expected_path(contract.provenance),
        "bytes": contract.provenance_pin.bytes,
        "sha256": contract.provenance_pin.sha256,
    }
    expected_audit = {
        "path": expected_path(contract.source_audit),
        "bytes": contract.source_audit_pin.bytes,
        "sha256": contract.source_audit_pin.sha256,
    }
    if card_provenance.get("preregistration") != expected_preregistration:
        raise KiBizOverlayError("KiBiz source-card preregistration binding drift")
    if card_provenance.get("source_audit") != expected_audit:
        raise KiBizOverlayError("KiBiz source-card source-audit binding drift")
    resources = provenance.get("metadata_observation_freeze", {}).get("resources")
    if not isinstance(resources, list):
        raise KiBizOverlayError("KiBiz primary terms resources are missing")
    terms = [
        value
        for value in resources
        if isinstance(value, dict) and value.get("role") == "iso_dataset"
    ]
    if len(terms) != 1:
        raise KiBizOverlayError("KiBiz primary terms resource is ambiguous")
    card_terms = card_provenance.get("terms_snapshot")
    if not isinstance(card_terms, dict):
        raise KiBizOverlayError("KiBiz source-card terms binding is missing")
    expected_terms = {
        "path": expected_path(contract.terms_snapshot),
        "bytes": contract.terms_snapshot_pin.bytes,
        "sha256": contract.terms_snapshot_pin.sha256,
    }
    if card_terms != expected_terms or (
        card_terms.get("bytes"), card_terms.get("sha256")
    ) != (terms[0].get("bytes"), terms[0].get("sha256")):
        raise KiBizOverlayError("KiBiz source-card primary terms binding drift")
    raw_source = source_card.get("raw_source")
    if not isinstance(raw_source, dict):
        raise KiBizOverlayError("KiBiz source-card raw-source binding is missing")
    if (raw_source.get("bytes"), raw_source.get("sha256"), raw_source.get("features")) != (
        source_audit.get("body_bytes"),
        source_audit.get("body_sha256"),
        source_audit.get("source_feature_count"),
    ):
        raise KiBizOverlayError("KiBiz source-card raw-source binding drift")


def _validate_primary_terms(pinned: builder.PinnedFile) -> None:
    pinned.stream.seek(0)
    try:
        raw = pinned.stream.read().decode("utf-8", errors="strict")
    except UnicodeDecodeError as exc:
        raise KiBizOverlayError("KiBiz primary terms snapshot is not strict UTF-8") from exc
    required = (
        "Datenlizenz Deutschland Namensnennung 2.0",
        LICENSE_URL,
        ATTRIBUTION,
    )
    if any(value not in raw for value in required):
        raise KiBizOverlayError("KiBiz primary terms text does not reproduce the license chain")


def _safe_projection(record: Mapping[str, Any], counts: defaultdict[str, int]) -> Projection | None:
    safe: dict[str, str] = {}
    for field_name in SAFE_ROSTER_FIELDS:
        value = record.get(field_name)
        if not isinstance(value, str) or "\x00" in value:
            raise KiBizOverlayError(f"KiBiz safe field {field_name!r} is malformed")
        safe[field_name] = value.strip()
    if safe["state_code"] != "DE-NW":
        counts["source_rows_rejected_state"] += 1
        return None
    street_norm = builder.normalize_text(safe["street_name"])
    locality_norm = builder.normalize_text(safe["locality"])
    if not street_norm:
        counts["source_rows_rejected_street"] += 1
        return None
    if not locality_norm:
        counts["source_rows_rejected_locality"] += 1
        return None
    matched = _POSITIVE_SINGLE_HOUSE.fullmatch(safe["house_designator"])
    if matched is None:
        counts["source_rows_rejected_house"] += 1
        return None
    number = int(matched.group(1))
    if number <= 0 or number > 0xFFFF_FFFF:
        counts["source_rows_rejected_house"] += 1
        return None
    postcode = safe["postal_code"]
    if _POSTCODE.fullmatch(postcode) is None:
        counts["source_rows_rejected_postcode"] += 1
        return None
    return Projection(street_norm, locality_norm, number, matched.group(2).lower(), postcode)


def load_projections(
    pinned: builder.PinnedFile,
    *,
    expected_rows: int,
) -> tuple[dict[tuple[str, str, int, str], Projection], dict[str, int]]:
    pinned.stream.seek(0)
    text = TextIOWrapperNoClose(pinned.stream)
    counts: defaultdict[str, int] = defaultdict(int)
    grouped: dict[tuple[str, str, int, str], list[Projection]] = defaultdict(list)
    try:
        for line_number, raw in enumerate(text, 1):
            if len(raw.encode("utf-8")) > MAX_JSONL_LINE_BYTES:
                raise KiBizOverlayError("KiBiz roster line exceeds the bounded limit")
            value = _strict_json(raw, f"KiBiz roster line {line_number}")
            if not isinstance(value, dict):
                raise KiBizOverlayError("KiBiz roster record must be an object")
            counts["source_rows_seen"] += 1
            projection = _safe_projection(value, counts)
            if projection is None:
                continue
            counts["source_rows_projected"] += 1
            grouped[projection.identity].append(projection)
    finally:
        text.detach()
    if counts["source_rows_seen"] != expected_rows:
        raise KiBizOverlayError(
            f"KiBiz row-count pin mismatch: got {counts['source_rows_seen']}, "
            f"expected {expected_rows}"
        )
    accepted: dict[tuple[str, str, int, str], Projection] = {}
    for identity in sorted(grouped):
        values = grouped[identity]
        postcodes = {value.postcode for value in values}
        if len(values) != 1:
            counts["source_ambiguous_semantic_identities"] += 1
            counts["source_ambiguous_semantic_rows"] += len(values)
            if len(postcodes) != 1:
                counts["source_conflicting_postcode_identities"] += 1
                counts["source_conflicting_postcode_rows"] += len(values)
            continue
        postcode = next(iter(postcodes))
        accepted[identity] = Projection(
            street_norm=identity[0],
            locality_norm=identity[1],
            number=identity[2],
            suffix=identity[3],
            postcode=postcode,
        )
    counts["accepted_source_identities"] = len(accepted)
    if not accepted:
        raise KiBizOverlayError("KiBiz roster produced no safe source identities")
    if pinned.evidence["sha256"] == FROZEN_ROSTER_PIN.sha256:
        frozen_checks = {
            "source_rows_seen": 10_906,
            "source_rows_projected": 10_408,
            "source_rows_rejected_house": 498,
            "source_ambiguous_semantic_identities": 8,
            "source_ambiguous_semantic_rows": 16,
            "accepted_source_identities": 10_392,
        }
        for key, expected in frozen_checks.items():
            if counts[key] != expected:
                raise KiBizOverlayError(f"frozen KiBiz source accounting drift at {key}")
    return accepted, dict(sorted(counts.items()))


class TextIOWrapperNoClose:
    """Strict UTF-8 iterator that preserves the caller-owned binary file."""

    def __init__(self, stream: Any) -> None:
        import io

        self._text = io.TextIOWrapper(stream, encoding="utf-8", errors="strict", newline="")

    def __iter__(self):
        return iter(self._text)

    def detach(self) -> Any:
        return self._text.detach()


def _csv_reader(pinned: builder.PinnedFile) -> tuple[TextIO, csv.DictReader]:
    pinned.stream.seek(0)
    try:
        text, reader = builder._csv_text_reader(
            pinned.stream, gzipped=pinned.path.suffix.lower() == ".gz"
        )
    except builder.SupplementError as exc:
        raise KiBizOverlayError(str(exc)) from exc
    if tuple(reader.fieldnames or ()) != builder.BUILDER_HEADER:
        text.close()
        raise KiBizOverlayError("builder CSV header drift")
    return text, reader


def _base_identity(row: Mapping[str, str]) -> tuple[str, str, int, str]:
    try:
        number = int(row["numero"])
    except (KeyError, TypeError, ValueError) as exc:
        raise KiBizOverlayError("builder row has invalid house number") from exc
    if number < 0 or number > 0xFFFF_FFFF:
        raise KiBizOverlayError("builder row house number is out of range")
    return (row["nom_voie_norm"], row["nom_commune_norm"], number, row["rep"])


def _postcode_state(row: Mapping[str, str]) -> tuple[str, bool, bool]:
    display = row["code_postal_display"].strip()
    numeric = row["code_postal"].strip()
    raw = [value for value in (display, numeric) if value]
    malformed = any(_POSTCODE.fullmatch(value) is None for value in raw)
    known = {value for value in raw if _POSTCODE.fullmatch(value) is not None}
    if len(known) > 1:
        malformed = True
    postcode = next(iter(known)) if len(known) == 1 else ""
    blank = not raw
    return postcode, blank, malformed


def scan_builder(
    pinned: builder.PinnedFile,
    projections: Mapping[tuple[str, str, int, str], Projection],
    *,
    expected_rows: int,
) -> tuple[dict[tuple[str, str, int, str], str], dict[str, int]]:
    observations: dict[tuple[str, str, int, str], BaseObservation] = {}
    counts: defaultdict[str, int] = defaultdict(int)
    text, reader = _csv_reader(pinned)
    try:
        for row in reader:
            counts["builder_rows_scanned"] += 1
            identity = _base_identity(row)
            if identity not in projections:
                continue
            observation = observations.setdefault(identity, BaseObservation())
            observation.rows += 1
            code = row["code_insee"].strip()
            if code:
                observation.codes.add(code)
            else:
                observation.malformed_locality_code = True
            postcode, blank, malformed = _postcode_state(row)
            if postcode:
                observation.postcodes.add(postcode)
            if blank:
                observation.blank_rows += 1
            if malformed:
                observation.malformed_postcode = True
            if observation.rows > MAX_MATCHED_IDENTITY_ROWS:
                raise KiBizOverlayError("matched builder identity exceeds bounded row limit")
    finally:
        text.close()
    if counts["builder_rows_scanned"] != expected_rows:
        raise KiBizOverlayError(
            f"builder row-count pin mismatch: got {counts['builder_rows_scanned']}, "
            f"expected {expected_rows}"
        )
    selected: dict[tuple[str, str, int, str], str] = {}
    for identity in sorted(projections):
        source = projections[identity]
        observation = observations.get(identity)
        if observation is None:
            counts["source_identities_missing_from_builder"] += 1
            continue
        if observation.malformed_locality_code or len(observation.codes) != 1:
            counts["source_identities_vetoed_locality_code"] += 1
            continue
        if observation.malformed_postcode or observation.postcodes - {source.postcode}:
            counts["source_identities_vetoed_base_postcode"] += 1
            continue
        if observation.blank_rows == 0:
            counts["source_identities_already_complete"] += 1
            continue
        selected[identity] = source.postcode
        counts["fillable_builder_identities"] += 1
        counts["fillable_builder_rows"] += observation.blank_rows
    if not selected:
        raise KiBizOverlayError("KiBiz postcode overlay is vacuous")
    return selected, dict(sorted(counts.items()))


def write_output(
    pinned: builder.PinnedFile,
    selected: Mapping[tuple[str, str, int, str], str],
    output: Path,
) -> dict[str, int]:
    counts: defaultdict[str, int] = defaultdict(int)
    text, reader = _csv_reader(pinned)
    try:
        try:
            with builder._canonical_gzip_writer(output, builder.BUILDER_HEADER) as writer:
                for row in reader:
                    identity = _base_identity(row)
                    postcode = selected.get(identity)
                    if postcode is not None:
                        _, blank, malformed = _postcode_state(row)
                        if malformed:
                            raise KiBizOverlayError("builder postcode changed between passes")
                        if blank:
                            projected = dict(row)
                            projected["code_postal"] = postcode
                            projected["code_postal_display"] = postcode
                            row = projected
                            counts["blank_postcode_rows_filled"] += 1
                    writer.writerow(row)
                    counts["base_rows_retained"] += 1
                    counts["output_rows"] += 1
        except builder.SupplementError as exc:
            raise KiBizOverlayError(str(exc)) from exc
    finally:
        text.close()
    return dict(sorted(counts.items()))


def _file_evidence(path: Path) -> dict[str, Any]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        while block := handle.read(8 * 2**20):
            digest.update(block)
            size += len(block)
    return {"path": str(path), "bytes": size, "sha256": digest.hexdigest()}


def _validate_config(config: Config) -> None:
    for label, pin in (
        ("KiBiz source card", config.source.source_card_pin),
        ("KiBiz roster", config.source.roster_pin),
        ("KiBiz provenance", config.source.provenance_pin),
        ("KiBiz source audit", config.source.source_audit_pin),
        ("KiBiz primary terms snapshot", config.source.terms_snapshot_pin),
        ("builder CSV", config.builder_pin),
    ):
        _validate_pin(pin, label)
    if config.expected_source_rows <= 0 or config.expected_builder_rows <= 0:
        raise KiBizOverlayError("expected row counts must be positive")
    if not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", config.source.snapshot_date):
        raise KiBizOverlayError("source snapshot date must be YYYY-MM-DD")
    if config.status != STATUS:
        raise KiBizOverlayError("unknown output status")
    if config.minimum_free_bytes < 0:
        raise KiBizOverlayError("minimum free bytes must be non-negative")
    inputs = (
        config.source.source_card,
        config.source.roster,
        config.source.provenance,
        config.source.source_audit,
        config.source.terms_snapshot,
        config.builder_csv,
    )
    outputs = (config.output_csv, config.receipt)
    for path in outputs:
        if path.exists() or path.is_symlink():
            raise KiBizOverlayError(f"refusing to overwrite output: {path}")
        if not path.parent.is_dir() or path.parent.is_symlink():
            raise KiBizOverlayError(
                f"output parent must be an existing real directory: {path.parent}"
            )
    resolved = [path.resolve(strict=True) for path in inputs]
    resolved.extend(path.resolve(strict=False) for path in outputs)
    if len(set(resolved)) != len(resolved):
        raise KiBizOverlayError("input and output paths must all be distinct")


def build(config: Config) -> dict[str, Any]:
    """Build the deterministic metadata overlay and its write-once receipt."""

    _validate_config(config)
    if shutil.disk_usage(config.output_csv.parent).free < config.minimum_free_bytes:
        raise KiBizOverlayError("disk floor crossed before KiBiz overlay build")
    source_card = _open_pinned(
        config.source.source_card, config.source.source_card_pin, "KiBiz source card"
    )
    roster: builder.PinnedFile | None = None
    provenance: builder.PinnedFile | None = None
    source_audit: builder.PinnedFile | None = None
    terms: builder.PinnedFile | None = None
    base: builder.PinnedFile | None = None
    try:
        roster = _open_pinned(config.source.roster, config.source.roster_pin, "KiBiz roster")
        provenance = _open_pinned(
            config.source.provenance, config.source.provenance_pin, "KiBiz provenance"
        )
        source_audit = _open_pinned(
            config.source.source_audit,
            config.source.source_audit_pin,
            "KiBiz source audit",
        )
        terms = _open_pinned(
            config.source.terms_snapshot,
            config.source.terms_snapshot_pin,
            "KiBiz primary terms snapshot",
        )
        base = _open_pinned(config.builder_csv, config.builder_pin, "builder CSV")
        _validate_primary_terms(terms)
        _validate_provenance(
            _read_pinned_json(source_card, "KiBiz source card"),
            _read_pinned_json(provenance, "KiBiz provenance"),
            _read_pinned_json(source_audit, "KiBiz source audit"),
            contract=config.source,
            expected_source_rows=config.expected_source_rows,
        )
        projections, source_counts = load_projections(
            roster, expected_rows=config.expected_source_rows
        )
        selected, scan_counts = scan_builder(
            base, projections, expected_rows=config.expected_builder_rows
        )
        write_counts = write_output(base, selected, config.output_csv)
        _recheck_pinned(source_card, "KiBiz source card")
        _recheck_pinned(roster, "KiBiz roster")
        _recheck_pinned(provenance, "KiBiz provenance")
        _recheck_pinned(source_audit, "KiBiz source audit")
        _recheck_pinned(terms, "KiBiz primary terms snapshot")
        _recheck_pinned(base, "builder CSV")
        counts = dict(sorted({**source_counts, **scan_counts, **write_counts}.items()))
        counts["source_rows_added"] = 0
        if counts.get("output_rows") != config.expected_builder_rows:
            raise KiBizOverlayError("overlay changed the builder row count")
        if counts.get("base_rows_retained") != config.expected_builder_rows:
            raise KiBizOverlayError("overlay dropped a base row")
        if counts.get("blank_postcode_rows_filled") != counts.get("fillable_builder_rows"):
            raise KiBizOverlayError("overlay fill count changed between passes")
        if counts.get("blank_postcode_rows_filled", 0) <= 0:
            raise KiBizOverlayError("KiBiz postcode overlay is vacuous")
        if counts["source_rows_added"] != 0:
            raise KiBizOverlayError("KiBiz metadata overlay added a source row")
        receipt: dict[str, Any] = {
            "schema": SCHEMA,
            "status": config.status,
            "license": {
                "data": LICENSE,
                "name": LICENSE_NAME,
                "url": LICENSE_URL,
                "attribution": f"{ATTRIBUTION} {config.source.snapshot_date}",
                "changes_marked": "GridPin metadata-only blank-postcode overlay",
            },
            "policy": {
                "outcome_blind_full_roster": True,
                "safe_source_fields": list(SAFE_ROSTER_FIELDS),
                "single_positive_house_only": True,
                "source_semantic_multiplicity_fail_closed": True,
                "exact_street_locality_house_suffix": True,
                "exactly_one_builder_locality_code": True,
                "base_nonblank_postcode_precedence": True,
                "source_rows_added": False,
                "source_coordinates_read": False,
                "source_coordinates_written": False,
                "base_coordinates_preserved": True,
                "base_row_order_preserved": True,
                "first_candidate_selection": False,
                "network_calls_during_build": 0,
                "gridpin_engine_calls_during_build": 0,
                "photon_engine_calls_during_build": 0,
            },
            "configuration": {
                "expected_source_rows": config.expected_source_rows,
                "expected_builder_rows": config.expected_builder_rows,
                "minimum_free_bytes": config.minimum_free_bytes,
                "maximum_roster_line_bytes": MAX_JSONL_LINE_BYTES,
                "maximum_matched_identity_rows": MAX_MATCHED_IDENTITY_ROWS,
                "single_house_grammar": _POSITIVE_SINGLE_HOUSE.pattern,
                "builder_header": list(builder.BUILDER_HEADER),
            },
            "inputs": {
                "source_card": dict(source_card.evidence),
                "post_prior_roster": dict(roster.evidence),
                "provenance": dict(provenance.evidence),
                "source_audit": dict(source_audit.evidence),
                "primary_terms_snapshot": dict(terms.evidence),
                "builder_csv": dict(base.evidence),
            },
            "counts": counts,
            "output": _file_evidence(config.output_csv),
        }
        with config.receipt.open("xb") as handle:
            handle.write(canonical_json_bytes(receipt))
            handle.flush()
            os.fsync(handle.fileno())
        return receipt
    finally:
        source_card.stream.close()
        for pinned in (roster, provenance, source_audit, terms, base):
            if pinned is not None:
                pinned.stream.close()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--builder-csv", type=Path, required=True)
    parser.add_argument("--builder-sha256", required=True)
    parser.add_argument("--builder-bytes", type=int, required=True)
    parser.add_argument("--expected-builder-rows", type=int, required=True)
    parser.add_argument("--output-csv", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--minimum-free-bytes", type=int, default=DEFAULT_MIN_FREE_BYTES)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    config = Config(
        source=SourceContract(
            source_card=FROZEN_SOURCE_CARD,
            source_card_pin=FROZEN_SOURCE_CARD_PIN,
            roster=FROZEN_ROSTER,
            roster_pin=FROZEN_ROSTER_PIN,
            provenance=FROZEN_PROVENANCE,
            provenance_pin=FROZEN_PROVENANCE_PIN,
            source_audit=FROZEN_SOURCE_AUDIT,
            source_audit_pin=FROZEN_SOURCE_AUDIT_PIN,
            terms_snapshot=FROZEN_TERMS_SNAPSHOT,
            terms_snapshot_pin=FROZEN_TERMS_SNAPSHOT_PIN,
            snapshot_date="2026-08-23",
        ),
        expected_source_rows=10_906,
        builder_csv=args.builder_csv,
        builder_pin=Pin(args.builder_sha256, args.builder_bytes),
        expected_builder_rows=args.expected_builder_rows,
        output_csv=args.output_csv,
        receipt=args.receipt,
        minimum_free_bytes=args.minimum_free_bytes,
    )
    try:
        receipt = build(config)
    except KiBizOverlayError as exc:
        raise SystemExit(f"DE KiBiz postcode overlay refused: {exc}") from exc
    print(json.dumps(receipt, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
