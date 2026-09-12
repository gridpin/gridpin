#!/usr/bin/env python3
"""Fill blank DE builder postcodes from a frozen lobid-organisations snapshot.

This overlay is deliberately raw-only and metadata-only.  Admission reads
only ``location[0].address`` values ``streetAddress``, ``postalCode`` and
``addressLocality`` from the byte-pinned bulk response.  It never consults a
source identifier, status, coordinate, contact field, benchmark result or
row ordinal, and it never adds a source row.  A postcode may only fill both
blank builder postcode fields on an exact existing
street/locality/single-house/suffix identity.  Every multiplicity or
conflicting base projection is fail-closed.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
from typing import Any

import de_kibiz_postcode_overlay as invariant
import de_photon_osm_supplement as builder


SCHEMA = "gridpin-de-isil-lobid-postcode-overlay-v1"
STATUS = "PUBLIC_PERMISSIVE_DEVELOPMENT"
SOURCE_CARD_SCHEMA = "gridpin-de-isil-lobid-source-card-v1"
LICENSE_FREEZE_SCHEMA = "gridpin-de-isil-lobid-license-freeze-v1"
LICENSE = "CC0-1.0"
LICENSE_NAME = "CC0 1.0 Universal"
LICENSE_URL = "https://creativecommons.org/publicdomain/zero/1.0/legalcode"
PUBLISHER = "Hochschulbibliothekszentrum des Landes Nordrhein-Westfalen (hbz)"
DATASET = "lobid-organisations"
DATASET_TITLE = "lobid-organisations — memory institutions in German-speaking countries"
DATASET_URL = "https://lobid.org/organisations"
SOURCE_URL = (
    "https://lobid.org/organisations/search?"
    "q=location.address.addressCountry%3ADE%20AND%20"
    "location.address.streetAddress%3A%2A%20AND%20"
    "location.address.postalCode%3A%2A%20AND%20isil%3A%2A&format=bulk"
)
FIXED_COUNTRY_CODE = "DE"
SAFE_SOURCE_VALUE_PATHS = (
    "location[0].address.streetAddress",
    "location[0].address.postalCode",
    "location[0].address.addressLocality",
)
DEFAULT_MIN_FREE_BYTES = 10 * 2**30
MAX_DECOMPRESSED_BYTES = 64 * 2**20
MAX_JSONL_LINE_BYTES = 1 * 2**20
EXPECTED_SOURCE_RECORDS = 18_414
_POSTCODE = re.compile(r"[0-9]{5}")
_STREET_HOUSE = re.compile(r"\A\s*(.+\S)\s+([0-9]+)\s*([A-Za-z]?)\s*\Z")
_SHA256 = re.compile(r"[0-9a-f]{64}")

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
FROZEN_SOURCE_CARD = REPOSITORY_ROOT / "code/prep/de_isil_lobid_source_v1.json"
FROZEN_LICENSE_FREEZE = REPOSITORY_ROOT / "code/eval/de_isil_lobid_license_freeze_v1.json"
FROZEN_RAW = (
    REPOSITORY_ROOT
    / "code/eval/work/de_f5_wave_a_isil_20260821/lobid_bulk_response.body"
)
FROZEN_HEADERS = (
    REPOSITORY_ROOT
    / "code/eval/work/de_f5_wave_a_isil_20260821/lobid_bulk_response.headers"
)
FROZEN_REQUEST = (
    REPOSITORY_ROOT
    / "code/eval/work/de_f5_wave_a_isil_20260821/request_preregistered.json"
)
FROZEN_TRANSFER = (
    REPOSITORY_ROOT / "code/eval/work/de_f5_wave_a_isil_20260821/transfer.json"
)
FROZEN_BNETZA_BUILDER = (
    REPOSITORY_ROOT
    / "code/eval/work/de_continuous_product_optimization_v1/"
    "bnetza_postcode_fill_only_v1/"
    "build_de_permissive_bnetza_postcode_fill_only_v1.csv.gz"
)


class IsilOverlayError(RuntimeError):
    """The ISIL metadata overlay cannot proceed without weakening a guard."""


@dataclass(frozen=True)
class Pin:
    sha256: str
    bytes: int


FROZEN_SOURCE_CARD_PIN = Pin(
    "96b264ff474de1b22654d048209904338fd73e94f59d46cbd7eafe30af9600f4",
    3_275,
)
FROZEN_LICENSE_FREEZE_PIN = Pin(
    "69bff549eb15102477c4dfc22931a2d1ab00976189e2b8cb95971d0c305a00f5",
    4_676,
)
FROZEN_RAW_PIN = Pin(
    "9622f042ccb7762c05c47b68ce0e2e37cadcc6e48b02e973e4dd88c480d05634",
    3_578_850,
)
FROZEN_HEADERS_PIN = Pin(
    "f56f40f065897c648010fccf9874e804640fbe1c50d26507def2134bd0419cdb",
    298,
)
FROZEN_REQUEST_PIN = Pin(
    "b78339fb868dcb95849828f8e66618b59189f6074f55fd0c35a0fddd4db0ebc4",
    3_292,
)
FROZEN_TRANSFER_PIN = Pin(
    "2a93e852012400b592771a3d5ab1a55165e7b64b3b92c730bfcb0f3fbdae9184",
    1_937,
)
FROZEN_BNETZA_BUILDER_PIN = Pin(
    "a40543295cfd261a2e76012b45d7d3712b2987abc09fa9ef6cbb1e4fb599f116",
    248_839_581,
)


@dataclass(frozen=True)
class SourceContract:
    source_card: Path
    source_card_pin: Pin
    license_freeze: Path
    license_freeze_pin: Pin
    raw_response: Path
    raw_response_pin: Pin
    response_headers: Path
    response_headers_pin: Pin
    request_preregistration: Path
    request_preregistration_pin: Pin
    transfer_receipt: Path
    transfer_receipt_pin: Pin
    snapshot_date: str


@dataclass(frozen=True)
class Config:
    source: SourceContract
    expected_source_records: int
    builder_csv: Path
    builder_pin: Pin
    expected_builder_rows: int
    output_csv: Path
    receipt: Path
    status: str = STATUS
    minimum_free_bytes: int = DEFAULT_MIN_FREE_BYTES


Projection = invariant.Projection


def canonical_json_bytes(value: Mapping[str, Any]) -> bytes:
    return invariant.canonical_json_bytes(value)


def _validate_pin(pin: Pin, label: str) -> None:
    if not isinstance(pin.sha256, str) or _SHA256.fullmatch(pin.sha256) is None:
        raise IsilOverlayError(f"{label} SHA-256 pin is invalid")
    if isinstance(pin.bytes, bool) or not isinstance(pin.bytes, int) or pin.bytes <= 0:
        raise IsilOverlayError(f"{label} byte pin must be positive")


def _builder_pin(pin: Pin) -> builder.Pin:
    return builder.Pin(sha256=pin.sha256, bytes=pin.bytes)


def _open_pinned(path: Path, pin: Pin, label: str) -> builder.PinnedFile:
    _validate_pin(pin, label)
    try:
        return builder._open_pinned(path, _builder_pin(pin), label)
    except builder.SupplementError as exc:
        raise IsilOverlayError(str(exc)) from exc


def _recheck_pinned(pinned: builder.PinnedFile, label: str) -> None:
    try:
        builder._recheck_pinned(pinned, label)
    except builder.SupplementError as exc:
        raise IsilOverlayError(str(exc)) from exc


def _json_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise IsilOverlayError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def _nonfinite_json(token: str) -> None:
    raise IsilOverlayError(f"non-finite JSON value is forbidden: {token}")


def _strict_json(raw: bytes, label: str) -> Any:
    try:
        return json.loads(
            raw.decode("utf-8", errors="strict"),
            object_pairs_hook=_json_pairs,
            parse_constant=_nonfinite_json,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise IsilOverlayError(f"{label}: invalid strict UTF-8 JSON: {exc}") from exc


def _read_json_object(pinned: builder.PinnedFile, label: str) -> Mapping[str, Any]:
    pinned.stream.seek(0)
    value = _strict_json(pinned.stream.read(), label)
    if not isinstance(value, dict):
        raise IsilOverlayError(f"{label} must be a JSON object")
    return value


def _path_text(path: Path) -> str:
    resolved = path.resolve(strict=True)
    try:
        return str(resolved.relative_to(REPOSITORY_ROOT))
    except ValueError:
        return str(resolved)


def _evidence(path: Path, pin: Pin) -> dict[str, Any]:
    return {"path": _path_text(path), "bytes": pin.bytes, "sha256": pin.sha256}


def _evidence_with_compression(path: Path, pin: Pin) -> dict[str, Any]:
    return {**_evidence(path, pin), "compression": "gzip"}


def _pin_from_evidence(value: Any, label: str) -> Pin:
    if not isinstance(value, dict):
        raise IsilOverlayError(f"{label} evidence is missing")
    pin = Pin(value.get("sha256"), value.get("bytes"))
    _validate_pin(pin, label)
    if not isinstance(value.get("path"), str) or not value["path"]:
        raise IsilOverlayError(f"{label} path is missing")
    return pin


def _path_from_evidence(value: Mapping[str, Any], label: str) -> Path:
    raw = value.get("path")
    if not isinstance(raw, str) or not raw:
        raise IsilOverlayError(f"{label} path is missing")
    path = Path(raw)
    if not path.is_absolute():
        path = REPOSITORY_ROOT / path
    try:
        return path.resolve(strict=True)
    except OSError as exc:
        raise IsilOverlayError(f"{label} path is unavailable") from exc


def _validate_source_documents(
    card: Mapping[str, Any],
    license_freeze: Mapping[str, Any],
    request: Mapping[str, Any],
    transfer: Mapping[str, Any],
    headers: str,
    contract: SourceContract,
    *,
    expected_source_records: int,
) -> list[tuple[str, Path, Pin]]:
    """Bind legal evidence and the raw snapshot without using source IDs."""

    if card.get("schema") != SOURCE_CARD_SCHEMA:
        raise IsilOverlayError("ISIL source-card schema drift")
    if card.get("publisher") != PUBLISHER or card.get("dataset") != {
        "bulk_endpoint": SOURCE_URL,
        "service": DATASET,
        "title": DATASET_TITLE,
    }:
        raise IsilOverlayError("ISIL source-card publisher/dataset drift")
    if card.get("license") != {
        "attribution_required": False,
        "identifier": LICENSE,
        "name": LICENSE_NAME,
        "url": LICENSE_URL,
    }:
        raise IsilOverlayError("ISIL source-card license drift")
    if card.get("license_freeze") != _evidence(
        contract.license_freeze, contract.license_freeze_pin
    ):
        raise IsilOverlayError("ISIL source-card license-freeze binding drift")
    if card.get("frozen_source") != {
        "raw_response": _evidence_with_compression(
            contract.raw_response, contract.raw_response_pin
        ),
        "response_headers": _evidence(
            contract.response_headers, contract.response_headers_pin
        ),
        "request_preregistration": _evidence(
            contract.request_preregistration, contract.request_preregistration_pin
        ),
        "transfer_receipt": _evidence(
            contract.transfer_receipt, contract.transfer_receipt_pin
        ),
        "records": expected_source_records,
        "snapshot_date": contract.snapshot_date,
    }:
        raise IsilOverlayError("ISIL source-card frozen-source binding drift")
    if card.get("safe_field_policy") != {
        "location_index": 0,
        "source_address_fields_accessed": [
            "streetAddress",
            "postalCode",
            "addressLocality",
        ],
        "fixed_country_code": FIXED_COUNTRY_CODE,
        "source_values_not_accessed": (
            "identifiers, organisation names/types/status, contact and web data, "
            "coordinates, source/lineage metadata and secondary locations"
        ),
    }:
        raise IsilOverlayError("ISIL source-card safe-projection policy drift")

    if license_freeze.get("schema") != LICENSE_FREEZE_SCHEMA:
        raise IsilOverlayError("ISIL license-freeze schema drift")
    if license_freeze.get("dataset") != {
        "bulk_endpoint": SOURCE_URL,
        "service": DATASET,
        "service_url": DATASET_URL,
        "sources": ["German ISIL registry", "German Library Statistics base data"],
        "title": DATASET_TITLE,
    }:
        raise IsilOverlayError("ISIL license-freeze dataset drift")
    if license_freeze.get("license") != {
        "attribution_required": False,
        "identifier": LICENSE,
        "name": LICENSE_NAME,
        "recommended_attribution": "powered by lobid data",
        "terms_url": LICENSE_URL,
    }:
        raise IsilOverlayError("ISIL license-freeze legal conclusion drift")
    if license_freeze.get("frozen_data") != {
        "request": _evidence(
            contract.request_preregistration, contract.request_preregistration_pin
        ),
        "response_headers": _evidence(
            contract.response_headers, contract.response_headers_pin
        ),
        "rows": expected_source_records,
        "snapshot": _evidence_with_compression(
            contract.raw_response, contract.raw_response_pin
        ),
        "snapshot_date": contract.snapshot_date,
        "transfer": _evidence(contract.transfer_receipt, contract.transfer_receipt_pin),
    }:
        raise IsilOverlayError("ISIL license-freeze frozen-data binding drift")
    verification = license_freeze.get("verification")
    if not isinstance(verification, dict) or any(
        verification.get(key) is not True
        for key in (
            "bulk_endpoint_bound",
            "bulk_format_documented",
            "cc0_applies_to_lobid_data",
            "organisations_data_served_through_documented_api",
        )
    ):
        raise IsilOverlayError("ISIL license-freeze verification drift")
    if not isinstance(verification.get("policy"), str) or not verification["policy"]:
        raise IsilOverlayError("ISIL license-freeze verification policy is missing")
    metadata = license_freeze.get("metadata_freeze")
    if not isinstance(metadata, dict):
        raise IsilOverlayError("ISIL license metadata freeze is missing")
    if metadata.get("network_calls") != 4 or metadata.get("raw_data_downloaded") is not False:
        raise IsilOverlayError("ISIL license metadata acquisition drift")
    if not re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z",
        str(metadata.get("retrieved_at_utc") or ""),
    ):
        raise IsilOverlayError("ISIL license metadata timestamp drift")
    resources = metadata.get("resources")
    if not isinstance(resources, list) or len(resources) != 4:
        raise IsilOverlayError("ISIL license-freeze primary evidence is incomplete")
    expected_roles = {
        "official_lobid_usage_policy": "https://lobid.org/usage-policy/",
        "official_lobid_organisations_dataset_page": DATASET_URL,
        "official_lobid_organisations_api_documentation": (
            "https://lobid.org/organisations/api/en"
        ),
        "cc0_1_0_legal_code": LICENSE_URL,
    }
    specs: list[tuple[str, Path, Pin]] = []
    seen_roles: set[str] = set()
    for resource in resources:
        if not isinstance(resource, dict):
            raise IsilOverlayError("ISIL license resource must be an object")
        role = resource.get("role")
        if not isinstance(role, str) or not role or role in seen_roles:
            raise IsilOverlayError("ISIL license resource role is invalid or duplicated")
        seen_roles.add(role)
        if resource.get("url") != expected_roles.get(role):
            raise IsilOverlayError("ISIL license resource URL/role drift")
        for kind in ("body", "headers"):
            evidence = resource.get(kind)
            label = f"{role} {kind}"
            pin = _pin_from_evidence(evidence, label)
            specs.append((label, _path_from_evidence(evidence, label), pin))
    if seen_roles != set(expected_roles):
        raise IsilOverlayError("ISIL license-freeze lacks required primary roles")

    if request.get("wave_id") != "DE-F5-REAL-A-ISIL-20260821":
        raise IsilOverlayError("ISIL request wave binding drift")
    request_doc = request.get("request")
    if not isinstance(request_doc, dict) or request_doc.get("method") != "GET":
        raise IsilOverlayError("ISIL request method drift")
    if request_doc.get("url") != SOURCE_URL or request_doc.get("retry_count") != 0:
        raise IsilOverlayError("ISIL request URL/retry drift")
    lineage = request.get("source_lineage")
    if not isinstance(lineage, dict) or lineage.get("license") != LICENSE_NAME:
        raise IsilOverlayError("ISIL request license lineage drift")
    if lineage.get("license_url") != "https://creativecommons.org/publicdomain/zero/1.0/":
        raise IsilOverlayError("ISIL request license URL drift")
    sampling = request.get("sampling")
    if not isinstance(sampling, dict) or any(value is not False for value in sampling.values()):
        raise IsilOverlayError("ISIL request is not outcome-blind and unsampled")

    if transfer.get("wave_id") != request.get("wave_id"):
        raise IsilOverlayError("ISIL transfer/request wave mismatch")
    transport = transfer.get("transport")
    artifacts = transfer.get("artifacts")
    immutability = transfer.get("immutability")
    if not isinstance(transport, dict) or transport.get("http_code") != 200:
        raise IsilOverlayError("ISIL transfer HTTP status drift")
    if (
        transport.get("url_effective") != SOURCE_URL
        or transport.get("content_type") != "application/x-jsonlines"
        or transport.get("content_encoding") != "gzip"
        or transport.get("size_download") != contract.raw_response_pin.bytes
        or transport.get("num_redirects") != 0
        or transport.get("ssl_verify_result") != 0
    ):
        raise IsilOverlayError("ISIL transfer transport drift")
    if not isinstance(artifacts, dict):
        raise IsilOverlayError("ISIL transfer artifact evidence is missing")
    raw_artifact = artifacts.get("raw_response")
    header_artifact = artifacts.get("raw_response_headers")
    if not isinstance(raw_artifact, dict) or not isinstance(header_artifact, dict):
        raise IsilOverlayError("ISIL transfer raw/header binding is missing")
    expected_raw = {
        "bytes": contract.raw_response_pin.bytes,
        "sha256": contract.raw_response_pin.sha256,
        "gzip_integrity": "ok",
        "decompressed_json_lines": expected_source_records,
    }
    if any(raw_artifact.get(key) != value for key, value in expected_raw.items()):
        raise IsilOverlayError("ISIL transfer raw binding drift")
    if any(
        header_artifact.get(key) != value
        for key, value in {
            "bytes": contract.response_headers_pin.bytes,
            "sha256": contract.response_headers_pin.sha256,
        }.items()
    ):
        raise IsilOverlayError("ISIL transfer header binding drift")
    if not isinstance(immutability, dict) or immutability.get(
        "destination_was_absent_before_fetch"
    ) is not True:
        raise IsilOverlayError("ISIL transfer was not create-only")
    if (
        immutability.get("request_attempts") != 1
        or immutability.get("retry_count") != 0
        or immutability.get("overwrite_performed") is not False
    ):
        raise IsilOverlayError("ISIL transfer immutability/retry drift")
    lower_headers = headers.lower()
    if "200 ok" not in lower_headers:
        raise IsilOverlayError("ISIL response header status drift")
    if "content-type: application/x-jsonlines" not in lower_headers:
        raise IsilOverlayError("ISIL response header content-type drift")
    if "content-encoding: gzip" not in lower_headers:
        raise IsilOverlayError("ISIL response header encoding drift")
    return specs


def _validate_metadata_contents(contents: Mapping[str, bytes]) -> None:
    expected_substrings = {
        "official_lobid_usage_policy body": (
            "Die lobid-Daten sind CC0-lizenziert",
            "keinerlei Auflagen oder Bedingungen",
            "keine Verpflichtung",
            "format=bulk",
        ),
        "official_lobid_organisations_dataset_page body": (
            "lobid-organisations ist ein umfassendes Verzeichnis",
            "Deutsche ISIL-Verzeichnis",
            "Stammdaten der",
            "Bibliotheksstatistik",
            "/organisations/api",
        ),
        "official_lobid_organisations_api_documentation body": (
            "API basics",
            "format=bulk",
            "Accept-Encoding: gzip",
        ),
        "cc0_1_0_legal_code body": (
            "CC0 1.0 Universal",
            "Waiver",
            "Affirmer",
        ),
    }
    for label, needles in expected_substrings.items():
        try:
            text = contents[label].decode("utf-8", errors="strict")
        except UnicodeDecodeError as exc:
            raise IsilOverlayError(f"{label} is not strict UTF-8") from exc
        if any(needle not in text for needle in needles):
            raise IsilOverlayError(f"{label} primary content drift")
    for label, payload in contents.items():
        if not label.endswith(" headers"):
            continue
        text = payload.decode("iso-8859-1", errors="strict").lower()
        first_line = text.splitlines()[0] if text.splitlines() else ""
        if re.fullmatch(r"http/(?:1\.[01]|2) 200(?: ok)?\s*", first_line) is None:
            raise IsilOverlayError(f"{label} HTTP status witness drift")
        if "content-type: text/html" not in text:
            raise IsilOverlayError(f"{label} HTTP witness drift")


def _safe_projection(
    record: Mapping[str, Any], counts: defaultdict[str, int]
) -> Projection | None:
    locations = record.get("location")
    if not isinstance(locations, list) or not locations or not isinstance(locations[0], dict):
        counts["source_rows_rejected_main_location"] += 1
        return None
    address = locations[0].get("address")
    if not isinstance(address, dict):
        counts["source_rows_rejected_address"] += 1
        return None
    street_line = address.get("streetAddress")
    postcode = address.get("postalCode")
    locality = address.get("addressLocality")
    if not all(isinstance(value, str) for value in (street_line, postcode, locality)):
        counts["source_rows_rejected_address_type"] += 1
        return None
    street_line = street_line.strip()
    postcode = postcode.strip()
    locality = locality.strip()
    if any("\x00" in value for value in (street_line, postcode, locality)):
        raise IsilOverlayError("ISIL safe source value contains NUL")
    if _POSTCODE.fullmatch(postcode) is None or postcode == "00000":
        counts["source_rows_rejected_postcode"] += 1
        return None
    locality_norm = builder.normalize_text(locality)
    if not locality_norm:
        counts["source_rows_rejected_locality"] += 1
        return None
    if re.search(r"\bpostfach\b", builder.normalize_text(street_line)) is not None:
        counts["source_rows_rejected_postfach"] += 1
        return None
    matched = _STREET_HOUSE.fullmatch(street_line)
    if matched is None:
        counts["source_rows_rejected_street_house"] += 1
        return None
    street_norm = builder.normalize_text(matched.group(1))
    number = int(matched.group(2))
    if not street_norm or number <= 0 or number > 0xFFFF_FFFF:
        counts["source_rows_rejected_street_house"] += 1
        return None
    return Projection(
        street_norm=street_norm,
        locality_norm=locality_norm,
        number=number,
        suffix=matched.group(3).lower(),
        postcode=postcode,
    )


def load_projections(
    pinned: builder.PinnedFile, *, expected_records: int
) -> tuple[dict[tuple[str, str, int, str], Projection], dict[str, int]]:
    """Read only the three safe main-address values from the pinned bulk body."""

    pinned.stream.seek(0)
    compressed = pinned.stream.read()
    if not compressed.startswith(b"\x1f\x8b"):
        raise IsilOverlayError("ISIL raw response is not gzip encoded")
    try:
        raw = gzip.decompress(compressed)
    except (EOFError, OSError) as exc:
        raise IsilOverlayError(f"ISIL raw gzip is invalid: {exc}") from exc
    if len(raw) > MAX_DECOMPRESSED_BYTES:
        raise IsilOverlayError("ISIL raw response exceeds decompressed byte limit")
    if not raw or not raw.endswith(b"\n"):
        raise IsilOverlayError("ISIL raw JSONL must end with a complete newline")
    lines = raw.splitlines(keepends=True)
    if len(lines) != expected_records:
        raise IsilOverlayError(
            f"ISIL record-count pin mismatch: got {len(lines)}, expected {expected_records}"
        )
    counts: defaultdict[str, int] = defaultdict(int)
    grouped: dict[tuple[str, str, int, str], list[str]] = defaultdict(list)
    for line_number, line in enumerate(lines, 1):
        counts["source_rows_seen"] += 1
        if len(line) > MAX_JSONL_LINE_BYTES:
            raise IsilOverlayError("ISIL JSONL row exceeds bounded limit")
        if not line.strip():
            raise IsilOverlayError(f"ISIL JSONL contains blank line {line_number}")
        value = _strict_json(line, f"ISIL record {line_number}")
        if not isinstance(value, dict):
            raise IsilOverlayError(f"ISIL record {line_number} must be an object")
        projection = _safe_projection(value, counts)
        if projection is None:
            continue
        counts["source_rows_projected"] += 1
        grouped[projection.identity].append(projection.postcode)

    accepted: dict[tuple[str, str, int, str], Projection] = {}
    counts["source_semantic_identities"] = len(grouped)
    for identity in sorted(grouped):
        postcodes = grouped[identity]
        if len(postcodes) != 1:
            counts["source_ambiguous_semantic_identities"] += 1
            counts["source_ambiguous_semantic_rows"] += len(postcodes)
            if len(set(postcodes)) != 1:
                counts["source_conflicting_postcode_identities"] += 1
                counts["source_conflicting_postcode_rows"] += len(postcodes)
            continue
        accepted[identity] = Projection(
            street_norm=identity[0],
            locality_norm=identity[1],
            number=identity[2],
            suffix=identity[3],
            postcode=postcodes[0],
        )
    counts["accepted_source_identities"] = len(accepted)
    if not accepted:
        raise IsilOverlayError("ISIL source produced no safe source identities")
    if pinned.evidence["sha256"] == FROZEN_RAW_PIN.sha256:
        frozen_checks = {
            "source_rows_seen": 18_414,
            "source_rows_projected": 15_414,
            "source_rows_rejected_postcode": 7,
            "source_rows_rejected_street_house": 2_993,
            "source_rows_rejected_postfach": 0,
            "source_semantic_identities": 11_448,
            "source_ambiguous_semantic_identities": 980,
            "source_ambiguous_semantic_rows": 4_946,
            "source_conflicting_postcode_identities": 44,
            "source_conflicting_postcode_rows": 272,
            "accepted_source_identities": 10_468,
        }
        for key, expected in frozen_checks.items():
            if counts[key] != expected:
                raise IsilOverlayError(f"frozen ISIL source accounting drift at {key}")
    return accepted, dict(sorted(counts.items()))


def scan_builder(
    pinned: builder.PinnedFile,
    projections: Mapping[tuple[str, str, int, str], Projection],
    *,
    expected_rows: int,
) -> tuple[dict[tuple[str, str, int, str], str], dict[str, int]]:
    try:
        selected, counts = invariant.scan_builder(
            pinned, projections, expected_rows=expected_rows
        )
    except invariant.KiBizOverlayError as exc:
        raise IsilOverlayError(str(exc).replace("KiBiz", "ISIL")) from exc
    if pinned.evidence["sha256"] == FROZEN_BNETZA_BUILDER_PIN.sha256:
        frozen_checks = {
            "builder_rows_scanned": 19_267_049,
            "source_identities_missing_from_builder": 7_772,
            "fillable_builder_identities": 1_958,
            "fillable_builder_rows": 2_091,
            "source_identities_already_complete": 724,
            "source_identities_vetoed_base_postcode": 14,
            "source_identities_vetoed_locality_code": 0,
        }
        for key, expected in frozen_checks.items():
            if counts.get(key, 0) != expected:
                raise IsilOverlayError(f"frozen ISIL builder accounting drift at {key}")
    return selected, counts


def write_output(
    pinned: builder.PinnedFile,
    selected: Mapping[tuple[str, str, int, str], str],
    output: Path,
) -> dict[str, int]:
    try:
        return invariant.write_output(pinned, selected, output)
    except invariant.KiBizOverlayError as exc:
        raise IsilOverlayError(str(exc).replace("KiBiz", "ISIL")) from exc


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
        ("ISIL source card", config.source.source_card_pin),
        ("ISIL license freeze", config.source.license_freeze_pin),
        ("ISIL raw response", config.source.raw_response_pin),
        ("ISIL response headers", config.source.response_headers_pin),
        ("ISIL request preregistration", config.source.request_preregistration_pin),
        ("ISIL transfer receipt", config.source.transfer_receipt_pin),
        ("builder CSV", config.builder_pin),
    ):
        _validate_pin(pin, label)
    if config.expected_source_records <= 0 or config.expected_builder_rows <= 0:
        raise IsilOverlayError("expected row counts must be positive")
    if not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", config.source.snapshot_date):
        raise IsilOverlayError("ISIL source snapshot date must be YYYY-MM-DD")
    if config.status != STATUS:
        raise IsilOverlayError("unknown output status")
    if config.minimum_free_bytes < 0:
        raise IsilOverlayError("minimum free bytes must be non-negative")
    inputs = (
        config.source.source_card,
        config.source.license_freeze,
        config.source.raw_response,
        config.source.response_headers,
        config.source.request_preregistration,
        config.source.transfer_receipt,
        config.builder_csv,
    )
    outputs = (config.output_csv, config.receipt)
    for path in outputs:
        if path.exists() or path.is_symlink():
            raise IsilOverlayError(f"refusing to overwrite output: {path}")
        if not path.parent.is_dir() or path.parent.is_symlink():
            raise IsilOverlayError(
                f"output parent must be an existing real directory: {path.parent}"
            )
    resolved = [path.resolve(strict=True) for path in inputs]
    resolved.extend(path.resolve(strict=False) for path in outputs)
    if len(set(resolved)) != len(resolved):
        raise IsilOverlayError("input and output paths must all be distinct")


def build(config: Config) -> dict[str, Any]:
    """Build the deterministic create-only blank-postcode overlay."""

    _validate_config(config)
    if shutil.disk_usage(config.output_csv.parent).free < config.minimum_free_bytes:
        raise IsilOverlayError("disk floor crossed before ISIL overlay build")
    specs = (
        ("ISIL source card", config.source.source_card, config.source.source_card_pin),
        ("ISIL license freeze", config.source.license_freeze, config.source.license_freeze_pin),
        ("ISIL raw response", config.source.raw_response, config.source.raw_response_pin),
        (
            "ISIL response headers",
            config.source.response_headers,
            config.source.response_headers_pin,
        ),
        (
            "ISIL request preregistration",
            config.source.request_preregistration,
            config.source.request_preregistration_pin,
        ),
        (
            "ISIL transfer receipt",
            config.source.transfer_receipt,
            config.source.transfer_receipt_pin,
        ),
        ("builder CSV", config.builder_csv, config.builder_pin),
    )
    opened: list[tuple[str, builder.PinnedFile]] = []
    metadata_opened: list[tuple[str, builder.PinnedFile]] = []
    try:
        for label, path, pin in specs:
            opened.append((label, _open_pinned(path, pin, label)))
        files = {label: pinned for label, pinned in opened}
        files["ISIL response headers"].stream.seek(0)
        headers = files["ISIL response headers"].stream.read().decode(
            "iso-8859-1", errors="strict"
        )
        metadata_specs = _validate_source_documents(
            _read_json_object(files["ISIL source card"], "ISIL source card"),
            _read_json_object(files["ISIL license freeze"], "ISIL license freeze"),
            _read_json_object(
                files["ISIL request preregistration"], "ISIL request preregistration"
            ),
            _read_json_object(files["ISIL transfer receipt"], "ISIL transfer receipt"),
            headers,
            config.source,
            expected_source_records=config.expected_source_records,
        )
        primary_paths = {pinned.path.resolve(strict=True) for _, pinned in opened}
        metadata_paths: set[Path] = set()
        for label, path, pin in metadata_specs:
            resolved = path.resolve(strict=True)
            if resolved in primary_paths or resolved in metadata_paths:
                raise IsilOverlayError("ISIL metadata evidence path is reused")
            metadata_paths.add(resolved)
            metadata_opened.append((label, _open_pinned(path, pin, label)))
        metadata_contents: dict[str, bytes] = {}
        for label, pinned in metadata_opened:
            pinned.stream.seek(0)
            metadata_contents[label] = pinned.stream.read()
        _validate_metadata_contents(metadata_contents)
        projections, source_counts = load_projections(
            files["ISIL raw response"], expected_records=config.expected_source_records
        )
        selected, scan_counts = scan_builder(
            files["builder CSV"], projections, expected_rows=config.expected_builder_rows
        )
        write_counts = write_output(files["builder CSV"], selected, config.output_csv)
        for label, pinned in (*opened, *metadata_opened):
            _recheck_pinned(pinned, label)
        counts = dict(sorted({**source_counts, **scan_counts, **write_counts}.items()))
        counts["source_rows_added"] = 0
        if counts.get("output_rows") != config.expected_builder_rows:
            raise IsilOverlayError("ISIL overlay changed the builder row count")
        if counts.get("base_rows_retained") != config.expected_builder_rows:
            raise IsilOverlayError("ISIL overlay dropped a base row")
        if counts.get("blank_postcode_rows_filled") != counts.get("fillable_builder_rows"):
            raise IsilOverlayError("ISIL fill count changed between passes")
        if counts.get("blank_postcode_rows_filled", 0) <= 0:
            raise IsilOverlayError("ISIL postcode overlay is vacuous")
        receipt: dict[str, Any] = {
            "schema": SCHEMA,
            "status": config.status,
            "license": {
                "data": LICENSE,
                "name": LICENSE_NAME,
                "url": LICENSE_URL,
                "attribution_required": False,
            },
            "policy": {
                "outcome_blind_full_raw_source": True,
                "safe_source_value_paths": list(SAFE_SOURCE_VALUE_PATHS),
                "fixed_country_code": FIXED_COUNTRY_CODE,
                "source_identifier_status_coordinate_contact_values_used": False,
                "strict_terminal_positive_single_house_only": True,
                "source_postfach_veto": True,
                "source_semantic_multiplicity_fail_closed": True,
                "exact_street_locality_house_suffix": True,
                "exactly_one_builder_locality_code": True,
                "base_nonblank_postcode_precedence": True,
                "source_rows_added": False,
                "source_coordinates_written": False,
                "base_coordinates_preserved": True,
                "base_row_order_preserved": True,
                "first_candidate_selection": False,
                "network_calls_during_build": 0,
                "gridpin_engine_calls_during_build": 0,
                "photon_engine_calls_during_build": 0,
            },
            "configuration": {
                "expected_source_records": config.expected_source_records,
                "expected_builder_rows": config.expected_builder_rows,
                "minimum_free_bytes": config.minimum_free_bytes,
                "maximum_decompressed_bytes": MAX_DECOMPRESSED_BYTES,
                "maximum_jsonl_line_bytes": MAX_JSONL_LINE_BYTES,
                "single_house_grammar": _STREET_HOUSE.pattern,
                "builder_header": list(builder.BUILDER_HEADER),
            },
            "inputs": {
                "source_card": dict(files["ISIL source card"].evidence),
                "license_freeze": dict(files["ISIL license freeze"].evidence),
                "raw_response": dict(files["ISIL raw response"].evidence),
                "response_headers": dict(files["ISIL response headers"].evidence),
                "request_preregistration": dict(
                    files["ISIL request preregistration"].evidence
                ),
                "transfer_receipt": dict(files["ISIL transfer receipt"].evidence),
                "license_chain": {
                    label: dict(pinned.evidence) for label, pinned in metadata_opened
                },
                "builder_csv": dict(files["builder CSV"].evidence),
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
        for _, pinned in (*opened, *metadata_opened):
            pinned.stream.close()


def frozen_source_contract() -> SourceContract:
    return SourceContract(
        source_card=FROZEN_SOURCE_CARD,
        source_card_pin=FROZEN_SOURCE_CARD_PIN,
        license_freeze=FROZEN_LICENSE_FREEZE,
        license_freeze_pin=FROZEN_LICENSE_FREEZE_PIN,
        raw_response=FROZEN_RAW,
        raw_response_pin=FROZEN_RAW_PIN,
        response_headers=FROZEN_HEADERS,
        response_headers_pin=FROZEN_HEADERS_PIN,
        request_preregistration=FROZEN_REQUEST,
        request_preregistration_pin=FROZEN_REQUEST_PIN,
        transfer_receipt=FROZEN_TRANSFER,
        transfer_receipt_pin=FROZEN_TRANSFER_PIN,
        snapshot_date="2026-08-21",
    )


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
    try:
        config = Config(
            source=frozen_source_contract(),
            expected_source_records=EXPECTED_SOURCE_RECORDS,
            builder_csv=args.builder_csv,
            builder_pin=Pin(args.builder_sha256, args.builder_bytes),
            expected_builder_rows=args.expected_builder_rows,
            output_csv=args.output_csv,
            receipt=args.receipt,
            minimum_free_bytes=args.minimum_free_bytes,
        )
        receipt = build(config)
    except IsilOverlayError as exc:
        raise SystemExit(f"DE ISIL/lobid postcode overlay refused: {exc}") from exc
    print(json.dumps(receipt, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
