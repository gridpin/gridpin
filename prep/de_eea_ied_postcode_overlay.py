#!/usr/bin/env python3
"""Fill blank DE builder postcodes from a pinned EEA IED/E-PRTR snapshot.

The source snapshot is a bounded, Germany-only, latest-year address projection.
It contains exactly six product-visible columns and no facility identifier,
name, coordinate, confidentiality value, benchmark outcome or row ordinal.
Only an exact existing street/locality/single-house/suffix identity can be
changed.  Source multiplicity and conflicting builder projections fail closed;
no source row or coordinate is ever admitted into the builder.
"""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
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


SCHEMA = "gridpin-de-eea-ied-postcode-overlay-v1"
STATUS = "PUBLIC_PERMISSIVE_DEVELOPMENT"
SOURCE_CARD_SCHEMA = "gridpin-de-eea-ied-eprtr-v16-source-card-v1"
LICENSE_FREEZE_SCHEMA = "gridpin-de-eea-ied-eprtr-v16-license-freeze-v1"
PUBLISHER = "European Environment Agency (EEA)"
DATASET_IDENTIFIER = "eea_t_ied-eprtr_p_2007-2024_v16_r00"
DATASET_TITLE = "Industrial Emissions Directive 2010/75/EU and European Pollutant Release and Transfer Register Regulation (EC) No 166/2006 - ver. 16.0 Feb. 2026 (Tabular data)"
DATASET_DOI = "10.2909/657ac3cb-affa-4295-a4a9-27b4f539adab"
RELEASE_UUID = "657ac3cb-affa-4295-a4a9-27b4f539adab"
EDITION = "16.00"
PUBLICATION_DATE = "2026-02-20"
TABLE = "[IED].[v1r2].[ProductionFacility]"
LICENSE = "CC-BY-4.0"
LICENSE_NAME = "Creative Commons Attribution 4.0 International"
LICENSE_URL = "https://creativecommons.org/licenses/by/4.0/"
LICENSE_BASIS = (
    "The frozen EEA legal notice applies CC-BY to EEA-held website materials "
    "unless a different condition is stated; the related Industrial Reporting "
    "metadata independently states CC BY 4.0 and EEA copyright."
)
ATTRIBUTION = (
    "Contains information from the European Environment Agency (EEA) Discodata "
    "IED fixed schema v1r2, acquired 2026-08-29, reused under the EEA CC-BY "
    "policy (https://www.eea.europa.eu/en/legal-notice). Related Industrial "
    "Reporting release metadata: edition 16, DOI "
    "10.2909/657ac3cb-affa-4295-a4a9-27b4f539adab, CC BY 4.0. Extracted and "
    "modified by GridPin as a Germany-only normalized address overlay; no exact "
    "byte equivalence to the v16 ACCDB package is asserted."
)
FIXED_COUNTRY_CODE = "DE"
FIXED_REPORTING_YEAR = 2024
SAFE_SOURCE_FIELDS = (
    "countryCode",
    "reportingYear",
    "streetName",
    "buildingNumber",
    "city",
    "postalCode",
)
DEFAULT_MIN_FREE_BYTES = 10 * 2**30
MAX_DECOMPRESSED_BYTES = 16 * 2**20
MAX_JSONL_LINE_BYTES = 256 * 2**10
EXPECTED_SOURCE_RECORDS = 10_470
_POSTCODE = re.compile(r"[0-9]{5}")
_POSITIVE_SINGLE_HOUSE = re.compile(r"\s*([0-9]+)\s*([A-Za-z]?)\s*")
_SHA256 = re.compile(r"[0-9a-f]{64}")

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
FROZEN_SOURCE_CARD = REPOSITORY_ROOT / "code/prep/de_eea_ied_eprtr_v16_source_v1.json"
FROZEN_LICENSE_FREEZE = (
    REPOSITORY_ROOT / "code/eval/de_eea_ied_eprtr_v16_license_freeze_v1.json"
)
FROZEN_ACQUISITION_RECEIPT = (
    REPOSITORY_ROOT / "code/eval/de_eea_ied_v16_acquisition_receipt_v1.json"
)
FROZEN_SNAPSHOT = (
    REPOSITORY_ROOT
    / "code/eval/work/de_eea_ied_v16_acquisition_20260830/"
    "de_2024_production_facility_address_v1r2.json"
)
FROZEN_RESPONSE_HEADERS = (
    REPOSITORY_ROOT
    / "code/eval/work/de_eea_ied_v16_acquisition_20260830/"
    "de_2024_production_facility_address_v1r2.response.headers.txt"
)
FROZEN_SNAPSHOT_PIN_SHA256 = (
    "d865bc2fdde93db2a19902bf4a34815ddb7fb2a4a25b9ef95d831434b10faf73"
)
FROZEN_BNETZA_BUILDER_PIN_SHA256 = (
    "a40543295cfd261a2e76012b45d7d3712b2987abc09fa9ef6cbb1e4fb599f116"
)


class EeaIedOverlayError(RuntimeError):
    """The overlay cannot proceed without weakening a guard."""


@dataclass(frozen=True)
class Pin:
    sha256: str
    bytes: int


FROZEN_SOURCE_CARD_PIN = Pin(
    "ca0198808e18447897edb2aff45e92cedd9b54395718be834fce6aa750b3e59f",
    7_720,
)
FROZEN_LICENSE_FREEZE_PIN = Pin(
    "a862371523ee049ff8d0961c73039094ed6b60b7f77fdf90e3e0dbf89efb11e0",
    6_418,
)
FROZEN_ACQUISITION_RECEIPT_PIN = Pin(
    "8296c8310394b693dbf2112d55e58dbc7ea1f0aa2b0af8e7a71e46888c0aceae",
    5_143,
)
FROZEN_SNAPSHOT_PIN = Pin(FROZEN_SNAPSHOT_PIN_SHA256, 1_412_566)
FROZEN_RESPONSE_HEADERS_PIN = Pin(
    "86ee9780d691658e1741500a1860dc533ceef66b96be18890c1439a5d6f6328a",
    609,
)


@dataclass(frozen=True)
class SourceContract:
    source_card: Path
    source_card_pin: Pin
    license_freeze: Path
    license_freeze_pin: Pin
    snapshot: Path
    snapshot_pin: Pin
    response_headers: Path
    response_headers_pin: Pin
    acquisition_receipt: Path
    acquisition_receipt_pin: Pin
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
        raise EeaIedOverlayError(f"{label} SHA-256 pin is invalid")
    if isinstance(pin.bytes, bool) or not isinstance(pin.bytes, int) or pin.bytes <= 0:
        raise EeaIedOverlayError(f"{label} byte pin must be positive")


def _builder_pin(pin: Pin) -> builder.Pin:
    return builder.Pin(sha256=pin.sha256, bytes=pin.bytes)


def _open_pinned(path: Path, pin: Pin, label: str) -> builder.PinnedFile:
    _validate_pin(pin, label)
    try:
        return builder._open_pinned(path, _builder_pin(pin), label)
    except builder.SupplementError as exc:
        raise EeaIedOverlayError(str(exc)) from exc


def _recheck_pinned(pinned: builder.PinnedFile, label: str) -> None:
    try:
        builder._recheck_pinned(pinned, label)
    except builder.SupplementError as exc:
        raise EeaIedOverlayError(str(exc)) from exc


def _json_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise EeaIedOverlayError(f"duplicate JSON key: {key}")
        value[key] = item
    return value


def _nonfinite_json(token: str) -> None:
    raise EeaIedOverlayError(f"non-finite JSON value is forbidden: {token}")


def _strict_json(raw: bytes, label: str) -> Any:
    try:
        return json.loads(
            raw.decode("utf-8", errors="strict"),
            object_pairs_hook=_json_pairs,
            parse_constant=_nonfinite_json,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise EeaIedOverlayError(f"{label}: invalid strict UTF-8 JSON: {exc}") from exc


def _read_json_object(pinned: builder.PinnedFile, label: str) -> Mapping[str, Any]:
    pinned.stream.seek(0)
    value = _strict_json(pinned.stream.read(), label)
    if not isinstance(value, dict):
        raise EeaIedOverlayError(f"{label} must be a JSON object")
    return value


def _path_text(path: Path) -> str:
    resolved = path.resolve(strict=True)
    try:
        return str(resolved.relative_to(REPOSITORY_ROOT))
    except ValueError:
        return str(resolved)


def _evidence(path: Path, pin: Pin) -> dict[str, Any]:
    return {"path": _path_text(path), "bytes": pin.bytes, "sha256": pin.sha256}


def _pin_from_evidence(value: Any, label: str) -> Pin:
    if not isinstance(value, dict):
        raise EeaIedOverlayError(f"{label} evidence is missing")
    pin = Pin(value.get("sha256"), value.get("bytes"))
    _validate_pin(pin, label)
    if not isinstance(value.get("path"), str) or not value["path"]:
        raise EeaIedOverlayError(f"{label} path is missing")
    return pin


def _path_from_evidence(value: Mapping[str, Any], label: str) -> Path:
    raw = value.get("path")
    if not isinstance(raw, str) or not raw:
        raise EeaIedOverlayError(f"{label} path is missing")
    path = Path(raw)
    if not path.is_absolute():
        path = REPOSITORY_ROOT / path
    try:
        return path.resolve(strict=True)
    except OSError as exc:
        raise EeaIedOverlayError(f"{label} path is unavailable") from exc


def _dataset_document() -> dict[str, str]:
    return {
        "api_source": TABLE,
        "service": "EEA Discodata",
        "title": "EEA Discodata IED ProductionFacility fixed schema v1r2",
        "related_catalogue_release": _related_release_document(),
    }


def _related_release_document() -> dict[str, str]:
    return {
        "dataset_identifier": DATASET_IDENTIFIER,
        "doi": DATASET_DOI,
        "edition": EDITION,
        "publication_date": PUBLICATION_DATE,
        "title": DATASET_TITLE,
    }


def _license_document() -> dict[str, Any]:
    return {
        "attribution_required": True,
        "basis": LICENSE_BASIS,
        "changes_marked": True,
        "copyright_holder": PUBLISHER,
        "identifier": LICENSE,
        "name": LICENSE_NAME,
        "url": LICENSE_URL,
    }


def _validate_source_documents(
    card: Mapping[str, Any],
    license_freeze: Mapping[str, Any],
    acquisition: Mapping[str, Any],
    headers: str,
    contract: SourceContract,
    *,
    expected_source_records: int,
) -> list[tuple[str, Path, Pin]]:
    """Bind the exact release/license and safe acquisition to the snapshot."""

    if card.get("schema") != SOURCE_CARD_SCHEMA or card.get("publisher") != PUBLISHER:
        raise EeaIedOverlayError("EEA source-card identity drift")
    if card.get("dataset") != _dataset_document():
        raise EeaIedOverlayError("EEA source-card dataset drift")
    if card.get("license") != _license_document() or card.get("attribution") != ATTRIBUTION:
        raise EeaIedOverlayError("EEA source-card license/attribution drift")
    if card.get("license_freeze") != _evidence(
        contract.license_freeze, contract.license_freeze_pin
    ):
        raise EeaIedOverlayError("EEA source-card license-freeze binding drift")
    frozen_source = card.get("frozen_source")
    if not isinstance(frozen_source, dict):
        raise EeaIedOverlayError("EEA source-card frozen source is missing")
    if frozen_source.get("acquisition_receipt") != _evidence(
        contract.acquisition_receipt, contract.acquisition_receipt_pin
    ):
        raise EeaIedOverlayError("EEA source-card acquisition binding drift")
    if frozen_source.get("raw_response") != {
        **_evidence(contract.snapshot, contract.snapshot_pin),
        "records": expected_source_records,
    }:
        raise EeaIedOverlayError("EEA source-card raw-response binding drift")
    if frozen_source.get("response_headers") != _evidence(
        contract.response_headers, contract.response_headers_pin
    ):
        raise EeaIedOverlayError("EEA source-card response-header binding drift")
    card_version = card.get("version_binding")
    if not isinstance(card_version, dict) or any(
        card_version.get(key) != expected
        for key, expected in {
            "canonical_api_table": TABLE,
            "related_catalogue_edition": EDITION,
            "schema": "v1r2",
        }.items()
    ):
        raise EeaIedOverlayError("EEA source-card version binding drift")
    safe_policy = card.get("safe_field_policy")
    if not isinstance(safe_policy, dict) or any(
        safe_policy.get(key) != expected
        for key, expected in {
            "confidentiality_fields_inspected_but_not_exported": [
                "addressConfidentiality",
                "facilityNameConfidentiality",
                "parentCompanyConfidentiality",
            ],
            "fixed_country_code": FIXED_COUNTRY_CODE,
            "fixed_reporting_year": FIXED_REPORTING_YEAR,
            "selected_fields": list(SAFE_SOURCE_FIELDS),
            "source_values_not_acquired": (
                "all source-row identifiers; coordinates and geometry; facility, "
                "parent-company and site names; activities, status, contacts, URLs, "
                "NUTS/RBD fields, benchmark data, competitor data and every other "
                "source column"
            ),
        }.items()
    ):
        raise EeaIedOverlayError("EEA source-card safe-field policy drift")

    if license_freeze.get("schema") != LICENSE_FREEZE_SCHEMA:
        raise EeaIedOverlayError("EEA license-freeze schema drift")
    frozen_dataset = license_freeze.get("dataset")
    if not isinstance(frozen_dataset, dict) or any(
        frozen_dataset.get(key) != expected
        for key, expected in {
            "dataset_identifier": DATASET_IDENTIFIER,
            "doi": DATASET_DOI,
            "edition": EDITION,
            "metadata_record_uuid": RELEASE_UUID,
            "publication_date": PUBLICATION_DATE,
            "publisher": PUBLISHER,
            "title": DATASET_TITLE,
            "version": 16,
        }.items()
    ):
        raise EeaIedOverlayError("EEA license-freeze dataset drift")
    frozen_license = license_freeze.get("license")
    if not isinstance(frozen_license, dict) or any(
        frozen_license.get(key) != expected
        for key, expected in {
            "attribution_required": True,
            "changes_must_be_marked": True,
            "copyright_holder": PUBLISHER,
            "identifier": LICENSE,
            "name": LICENSE_NAME,
            "terms_url": LICENSE_URL,
        }.items()
    ):
        raise EeaIedOverlayError("EEA license-freeze legal conclusion drift")
    freeze_metadata = license_freeze.get("metadata_freeze")
    if not isinstance(freeze_metadata, dict):
        raise EeaIedOverlayError("EEA license metadata freeze is missing")
    resources = freeze_metadata.get("resources")
    expected_roles = {
        "official_eea_iso_metadata_and_license_binding",
        "official_eea_database_structure_and_confidentiality_document",
        "official_eea_distribution_readme",
        "official_eea_general_legal_notice",
    }
    if not isinstance(resources, list) or len(resources) != len(expected_roles):
        raise EeaIedOverlayError("EEA primary license evidence is incomplete")
    specs: list[tuple[str, Path, Pin]] = []
    roles: set[str] = set()
    for resource in resources:
        if not isinstance(resource, dict):
            raise EeaIedOverlayError("EEA primary resource must be an object")
        role = resource.get("role")
        if role not in expected_roles or role in roles:
            raise EeaIedOverlayError("EEA primary resource role drift")
        roles.add(role)
        if not isinstance(resource.get("url"), str) or not resource["url"].startswith(
            "https://"
        ):
            raise EeaIedOverlayError("EEA primary resource URL drift")
        evidence = resource.get("body")
        label = f"{role} body"
        specs.append(
            (
                label,
                _path_from_evidence(evidence, label),
                _pin_from_evidence(evidence, label),
            )
        )
        if role == "official_eea_general_legal_notice":
            header_evidence = resource.get("headers")
            header_label = f"{role} headers"
            specs.append(
                (
                    header_label,
                    _path_from_evidence(header_evidence, header_label),
                    _pin_from_evidence(header_evidence, header_label),
                )
            )
    if roles != expected_roles:
        raise EeaIedOverlayError("EEA primary license roles drift")

    if acquisition.get("schema") != "gridpin-de-eea-ied-v16-acquisition-receipt-v1":
        raise EeaIedOverlayError("EEA acquisition schema drift")
    request = acquisition.get("acquisition")
    if not isinstance(request, dict):
        raise EeaIedOverlayError("EEA acquisition request is missing")
    if (
        request.get("endpoint") != "https://discodata.eea.europa.eu/sql"
        or request.get("http_method") != "GET"
        or request.get("page") != 1
        or request.get("page_size") < expected_source_records
    ):
        raise EeaIedOverlayError("EEA acquisition request drift")
    sql = request.get("sql")
    if not isinstance(sql, str) or any(
        token not in sql
        for token in (
            TABLE,
            "countryCode = 'DE'",
            "reportingYear = 2024",
            "addressConfidentiality IS NULL",
            "facilityNameConfidentiality IS NULL",
            "parentCompanyConfidentiality IS NULL",
        )
    ):
        raise EeaIedOverlayError("EEA acquisition SQL scope drift")
    card_api = frozen_source.get("api")
    if not isinstance(card_api, dict) or any(
        card_api.get(key) != expected
        for key, expected in {
            "endpoint": "https://discodata.eea.europa.eu/sql",
            "method": "GET",
        }.items()
    ):
        raise EeaIedOverlayError("EEA source-card API binding drift")
    query_parameters = card_api.get("query_parameters")
    if not isinstance(query_parameters, dict) or query_parameters != {
        "nrOfHits": request.get("page_size"),
        "p": request.get("page"),
        "query": sql,
    }:
        raise EeaIedOverlayError("EEA source-card query binding drift")
    response_contract = card_api.get("response_contract")
    if not isinstance(response_contract, dict) or response_contract != {
        "expected_result_fields": list(SAFE_SOURCE_FIELDS),
        "results_path": "$.results",
        "top_level_type": "object",
    }:
        raise EeaIedOverlayError("EEA source-card response contract drift")
    if frozen_source.get("retrieved_at_utc") != request.get("retrieved_at_utc"):
        raise EeaIedOverlayError("EEA source-card acquisition time drift")
    frozen = acquisition.get("frozen_response")
    if not isinstance(frozen, dict) or frozen != {
        "body": _evidence(contract.snapshot, contract.snapshot_pin),
        "headers": _evidence(contract.response_headers, contract.response_headers_pin),
        "results": expected_source_records,
        "top_level_keys": ["results"],
    }:
        raise EeaIedOverlayError("EEA acquisition response binding drift")
    if acquisition.get("scope") != {
        "country_code": FIXED_COUNTRY_CODE,
        "reporting_year": FIXED_REPORTING_YEAR,
        "selected_fields": list(SAFE_SOURCE_FIELDS),
        "source_fields_not_acquired": (
            "all identifiers, coordinates, facility/site/installation names, "
            "parent-company values, activity/emission data, contact values and "
            "every other source column"
        ),
        "source_table": TABLE,
    }:
        raise EeaIedOverlayError("EEA acquisition safe projection drift")
    validation = acquisition.get("validation")
    if not isinstance(validation, dict) or any(
        validation.get(key) != expected
        for key, expected in {
            "all_rows_have_exact_selected_schema": True,
            "blank_selected_values": 0,
            "countries": [FIXED_COUNTRY_CODE],
            "five_digit_postcode_rows": expected_source_records,
            "years": [FIXED_REPORTING_YEAR],
        }.items()
    ):
        raise EeaIedOverlayError("EEA acquisition validation drift")
    count_witness = acquisition.get("fixed_schema_count_witness")
    if not isinstance(count_witness, dict) or count_witness.get("results") != [
        {"eligible_rows": expected_source_records, "total_rows": 10_513}
    ]:
        raise EeaIedOverlayError("EEA fixed-schema count witness drift")
    count_sql = count_witness.get("sql")
    if not isinstance(count_sql, str) or TABLE not in count_sql:
        raise EeaIedOverlayError("EEA fixed-schema count SQL drift")
    for kind in ("body", "headers"):
        evidence = count_witness.get(kind)
        label = f"fixed_schema_count_witness {kind}"
        specs.append(
            (
                label,
                _path_from_evidence(evidence, label),
                _pin_from_evidence(evidence, label),
            )
        )
    version = acquisition.get("version_binding")
    if not isinstance(version, dict) or version.get("fixed_schema") != "v1r2":
        raise EeaIedOverlayError("EEA fixed version binding drift")
    catalog = version.get("discodata_metadata_catalog")
    latest = version.get("latest_comparison")
    if (
        not isinstance(catalog, dict)
        or catalog.get("endpoint") != "https://discodata.eea.europa.eu/md"
        or "v1r2" not in catalog.get("production_facility_schemas", [])
        or not isinstance(latest, dict)
        or latest.get("safe_row_multiset_equal_to_fixed_schema") is not True
    ):
        raise EeaIedOverlayError("EEA version/equality witness drift")
    card_schema_evidence = card.get("schema_binding_evidence")
    card_equivalence = (
        card_schema_evidence.get("safe_field_multiset_equivalence")
        if isinstance(card_schema_evidence, dict)
        else None
    )
    if (
        not isinstance(card_schema_evidence, dict)
        or card_schema_evidence.get("catalogue_snapshot") != catalog.get("body")
        or card_schema_evidence.get("catalogue_snapshot_headers")
        != catalog.get("headers")
        or card_schema_evidence.get("observed_tables")
        != ["[IED].[latest].[ProductionFacility]", TABLE]
        or not isinstance(card_equivalence, dict)
        or card_equivalence.get("equal") is not True
        or card_equivalence.get("fields") != list(SAFE_SOURCE_FIELDS)
        or card_equivalence.get("fixed_v1r2_rows") != expected_source_records
        or card_equivalence.get("latest_rows") != expected_source_records
        or card_equivalence.get("latest_witness") != latest.get("body")
    ):
        raise EeaIedOverlayError("EEA source-card schema/equality evidence drift")
    for prefix, document in (
        ("discodata_metadata_catalog", catalog),
        ("latest_safe_multiset_comparison", latest),
    ):
        for kind in ("body", "headers"):
            evidence = document.get(kind)
            label = f"{prefix} {kind}"
            specs.append(
                (
                    label,
                    _path_from_evidence(evidence, label),
                    _pin_from_evidence(evidence, label),
                )
            )
    first_line = headers.lower().splitlines()[0] if headers.splitlines() else ""
    if re.fullmatch(r"http/(?:1\.[01]|2) 200(?: ok)?\s*", first_line) is None:
        raise EeaIedOverlayError("EEA response status witness drift")
    return specs


def _catalog_production_facility(
    value: Any, schema_id: str
) -> tuple[Mapping[str, Any], Mapping[str, Any], dict[str, str]]:
    if not isinstance(value, list):
        raise EeaIedOverlayError("EEA Discodata catalogue must be a list")
    schemas: list[Mapping[str, Any]] = []
    for database in value:
        if not isinstance(database, dict):
            continue
        for schema in database.get("Schemas", []):
            if isinstance(schema, dict) and schema.get("id") == schema_id:
                schemas.append(schema)
    if len(schemas) != 1:
        raise EeaIedOverlayError(f"EEA catalogue schema {schema_id} is ambiguous")
    schema = schemas[0]
    table_id = f"{schema_id}.[ProductionFacility]"
    tables = [
        table
        for table in schema.get("Tables", [])
        if isinstance(table, dict) and table.get("id") == table_id
    ]
    if len(tables) != 1:
        raise EeaIedOverlayError(f"EEA catalogue table {table_id} is ambiguous")
    table = tables[0]
    columns: dict[str, str] = {}
    for column in table.get("Columns", []):
        if not isinstance(column, dict):
            raise EeaIedOverlayError("EEA catalogue column is malformed")
        name = column.get("name")
        data_type = column.get("dataType")
        if not isinstance(name, str) or not isinstance(data_type, str) or name in columns:
            raise EeaIedOverlayError("EEA catalogue column identity is malformed")
        columns[name] = data_type
    return schema, table, columns


def _validate_catalogue(payload: bytes) -> None:
    catalogue = _strict_json(payload, "EEA Discodata metadata catalogue")
    fixed_schema, fixed, fixed_columns = _catalog_production_facility(
        catalogue, "[IED].[v1r2]"
    )
    _, latest, latest_columns = _catalog_production_facility(catalogue, "[IED].[latest]")
    if (
        fixed_schema.get("isRelease") is not True
        or fixed.get("datalakeSync") is not True
        or fixed.get("tableType") != "table"
        or fixed.get("isProxy") is not False
        or latest.get("datalakeSync") is not True
        or latest.get("tableType") != "view"
        or latest.get("isProxy") is not True
        or latest.get("proxyFor") != TABLE
    ):
        raise EeaIedOverlayError("EEA ProductionFacility catalogue binding drift")
    if len(fixed_columns) != 36 or fixed_columns != latest_columns:
        raise EeaIedOverlayError("EEA ProductionFacility 36-column schema drift")
    required_types = {
        "addressConfidentiality": "varchar",
        "buildingNumber": "nvarchar",
        "city": "nvarchar",
        "countryCode": "varchar",
        "facilityNameConfidentiality": "varchar",
        "parentCompanyConfidentiality": "varchar",
        "postalCode": "nvarchar",
        "reportingYear": "int",
        "streetName": "nvarchar",
    }
    if any(fixed_columns.get(name) != data_type for name, data_type in required_types.items()):
        raise EeaIedOverlayError("EEA ProductionFacility safe-column type drift")


def _safe_row_multiset(payload: bytes, *, expected_records: int) -> Counter[tuple[Any, ...]]:
    rows = _snapshot_rows(payload, expected_records=expected_records)
    result: Counter[tuple[Any, ...]] = Counter()
    for row in rows:
        if set(row) != set(SAFE_SOURCE_FIELDS) or len(row) != len(SAFE_SOURCE_FIELDS):
            raise EeaIedOverlayError("EEA equality witness contains forbidden fields")
        result[tuple(row[field] for field in SAFE_SOURCE_FIELDS)] += 1
    return result


def _validate_primary_contents(
    contents: Mapping[str, bytes],
    *,
    canonical_snapshot: bytes,
    expected_records: int,
) -> None:
    required = {
        "official_eea_iso_metadata_and_license_binding body": (
            RELEASE_UUID.encode(),
            EDITION.encode(),
            b"CC-BY 4.0",
        ),
        "official_eea_distribution_readme body": (
            b"Industrial Emissions Directive",
            b"E-PRTR",
            DATASET_DOI.encode(),
        ),
        "official_eea_general_legal_notice body": (
            b"EEA materials are published under the",
            b"CC-BY license",
            b"commercial or non-commercial purposes",
            b"acknowledged as the original source",
            b"original meaning or message of the content is not distorted",
        ),
    }
    for label, needles in required.items():
        if any(needle not in contents[label] for needle in needles):
            raise EeaIedOverlayError(f"{label} primary content drift")
    pdf_label = "official_eea_database_structure_and_confidentiality_document body"
    if not contents[pdf_label].startswith(b"%PDF-"):
        raise EeaIedOverlayError("EEA official confidentiality PDF drift")
    legal_headers = contents["official_eea_general_legal_notice headers"].decode(
        "iso-8859-1", errors="strict"
    )
    first_line = legal_headers.lower().splitlines()[0] if legal_headers.splitlines() else ""
    if re.fullmatch(r"http/(?:1\.[01]|2) 200(?: ok)?\s*", first_line) is None:
        raise EeaIedOverlayError("EEA legal-notice HTTP witness drift")
    for label, payload in contents.items():
        if not label.endswith(" headers"):
            continue
        text = payload.decode("iso-8859-1", errors="strict")
        status = text.lower().splitlines()[0] if text.splitlines() else ""
        if re.fullmatch(r"http/(?:1\.[01]|2) 200(?: ok)?\s*", status) is None:
            raise EeaIedOverlayError(f"{label} HTTP status witness drift")
    count_value = _strict_json(
        contents["fixed_schema_count_witness body"], "EEA fixed-schema count body"
    )
    if count_value != {
        "results": [{"eligible_rows": expected_records, "total_rows": 10_513}]
    }:
        raise EeaIedOverlayError("EEA fixed-schema count body drift")
    _validate_catalogue(contents["discodata_metadata_catalog body"])
    canonical = _safe_row_multiset(
        canonical_snapshot, expected_records=expected_records
    )
    latest = _safe_row_multiset(
        contents["latest_safe_multiset_comparison body"],
        expected_records=expected_records,
    )
    if canonical != latest:
        raise EeaIedOverlayError("EEA latest/fixed safe-row multiset mismatch")


def _safe_projection(
    record: Mapping[str, Any], counts: defaultdict[str, int]
) -> Projection | None:
    if set(record) != set(SAFE_SOURCE_FIELDS) or len(record) != len(SAFE_SOURCE_FIELDS):
        raise EeaIedOverlayError("EEA row contains missing or forbidden source fields")
    country = record.get("countryCode")
    year = record.get("reportingYear")
    street = record.get("streetName")
    house = record.get("buildingNumber")
    locality = record.get("city")
    postcode = record.get("postalCode")
    if country != FIXED_COUNTRY_CODE:
        counts["source_rows_rejected_country"] += 1
        return None
    if year != FIXED_REPORTING_YEAR:
        counts["source_rows_rejected_reporting_year"] += 1
        return None
    if not all(isinstance(value, str) for value in (street, house, locality, postcode)):
        counts["source_rows_rejected_address_type"] += 1
        return None
    street = street.strip()
    house = house.strip()
    locality = locality.strip()
    postcode = postcode.strip()
    if any("\x00" in value for value in (street, house, locality, postcode)):
        raise EeaIedOverlayError("EEA safe source value contains NUL")
    street_norm = builder.normalize_text(street)
    locality_norm = builder.normalize_text(locality)
    if not street_norm:
        counts["source_rows_rejected_street"] += 1
        return None
    if not locality_norm:
        counts["source_rows_rejected_locality"] += 1
        return None
    matched = _POSITIVE_SINGLE_HOUSE.fullmatch(house)
    if matched is None:
        counts["source_rows_rejected_house"] += 1
        return None
    number = int(matched.group(1))
    if number <= 0 or number > 0xFFFF_FFFF:
        counts["source_rows_rejected_house"] += 1
        return None
    if _POSTCODE.fullmatch(postcode) is None or postcode == "00000":
        counts["source_rows_rejected_postcode"] += 1
        return None
    return Projection(
        street_norm=street_norm,
        locality_norm=locality_norm,
        number=number,
        suffix=matched.group(2).lower(),
        postcode=postcode,
    )


def _snapshot_rows(payload: bytes, *, expected_records: int) -> list[Mapping[str, Any]]:
    if payload.startswith(b"\x1f\x8b"):
        try:
            payload = gzip.decompress(payload)
        except (EOFError, OSError) as exc:
            raise EeaIedOverlayError(f"EEA snapshot gzip is invalid: {exc}") from exc
    if len(payload) > MAX_DECOMPRESSED_BYTES:
        raise EeaIedOverlayError("EEA snapshot exceeds decompressed byte limit")
    value = _strict_json(payload, "EEA snapshot")
    if not isinstance(value, dict) or set(value) != {"results"}:
        raise EeaIedOverlayError("EEA snapshot top level must be exactly results")
    rows = value.get("results")
    if not isinstance(rows, list):
        raise EeaIedOverlayError("EEA snapshot results must be an array")
    if len(rows) != expected_records:
        raise EeaIedOverlayError(
            f"EEA record-count pin mismatch: got {len(rows)}, expected {expected_records}"
        )
    for index, row in enumerate(rows, 1):
        if not isinstance(row, dict):
            raise EeaIedOverlayError(f"EEA row {index} must be an object")
    return rows


def load_projections(
    pinned: builder.PinnedFile, *, expected_records: int
) -> tuple[dict[tuple[str, str, int, str], Projection], dict[str, int]]:
    pinned.stream.seek(0)
    rows = _snapshot_rows(pinned.stream.read(), expected_records=expected_records)
    counts: defaultdict[str, int] = defaultdict(int)
    grouped: dict[tuple[str, str, int, str], list[str]] = defaultdict(list)
    for row in rows:
        counts["source_rows_seen"] += 1
        projection = _safe_projection(row, counts)
        if projection is None:
            continue
        counts["source_rows_projected"] += 1
        grouped[projection.identity].append(projection.postcode)
    counts["source_semantic_identities"] = len(grouped)
    accepted: dict[tuple[str, str, int, str], Projection] = {}
    for identity in sorted(grouped):
        postcodes = grouped[identity]
        if len(postcodes) != 1:
            counts["source_ambiguous_semantic_identities"] += 1
            counts["source_ambiguous_semantic_rows"] += len(postcodes)
            if len(set(postcodes)) != 1:
                counts["source_conflicting_postcode_identities"] += 1
                counts["source_conflicting_postcode_rows"] += len(postcodes)
            continue
        accepted[identity] = Projection(*identity, postcode=postcodes[0])
    counts["accepted_source_identities"] = len(accepted)
    if not accepted:
        raise EeaIedOverlayError("EEA snapshot produced no safe source identities")
    if pinned.evidence["sha256"] == FROZEN_SNAPSHOT_PIN_SHA256:
        expected = {
            "source_rows_seen": 10_470,
            "source_rows_projected": 8_458,
            "source_rows_rejected_house": 1_901,
            "source_rows_rejected_street": 111,
            "source_semantic_identities": 8_016,
            "source_ambiguous_semantic_identities": 304,
            "source_ambiguous_semantic_rows": 746,
            "accepted_source_identities": 7_712,
        }
        for key, expected_value in expected.items():
            if counts[key] != expected_value:
                raise EeaIedOverlayError(f"frozen EEA source accounting drift at {key}")
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
        raise EeaIedOverlayError(str(exc).replace("KiBiz", "EEA")) from exc
    if pinned.evidence["sha256"] == FROZEN_BNETZA_BUILDER_PIN_SHA256:
        expected = {
            "builder_rows_scanned": 19_267_049,
            "source_identities_missing_from_builder": 4_477,
            "fillable_builder_identities": 2_564,
            "fillable_builder_rows": 2_658,
            "source_identities_already_complete": 650,
            "source_identities_vetoed_base_postcode": 20,
            "source_identities_vetoed_locality_code": 1,
        }
        for key, expected_value in expected.items():
            if counts.get(key, 0) != expected_value:
                raise EeaIedOverlayError(f"frozen EEA builder accounting drift at {key}")
    return selected, counts


def write_output(
    pinned: builder.PinnedFile,
    selected: Mapping[tuple[str, str, int, str], str],
    output: Path,
) -> dict[str, int]:
    try:
        return invariant.write_output(pinned, selected, output)
    except invariant.KiBizOverlayError as exc:
        raise EeaIedOverlayError(str(exc).replace("KiBiz", "EEA")) from exc


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
        ("EEA source card", config.source.source_card_pin),
        ("EEA license freeze", config.source.license_freeze_pin),
        ("EEA snapshot", config.source.snapshot_pin),
        ("EEA response headers", config.source.response_headers_pin),
        ("EEA acquisition receipt", config.source.acquisition_receipt_pin),
        ("builder CSV", config.builder_pin),
    ):
        _validate_pin(pin, label)
    if config.expected_source_records <= 0 or config.expected_builder_rows <= 0:
        raise EeaIedOverlayError("expected row counts must be positive")
    if not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", config.source.snapshot_date):
        raise EeaIedOverlayError("EEA snapshot date must be YYYY-MM-DD")
    if config.status != STATUS:
        raise EeaIedOverlayError("unknown output status")
    if config.minimum_free_bytes < 0:
        raise EeaIedOverlayError("minimum free bytes must be non-negative")
    inputs = (
        config.source.source_card,
        config.source.license_freeze,
        config.source.snapshot,
        config.source.response_headers,
        config.source.acquisition_receipt,
        config.builder_csv,
    )
    outputs = (config.output_csv, config.receipt)
    for path in outputs:
        if path.exists() or path.is_symlink():
            raise EeaIedOverlayError(f"refusing to overwrite output: {path}")
        if not path.parent.is_dir() or path.parent.is_symlink():
            raise EeaIedOverlayError(
                f"output parent must be an existing real directory: {path.parent}"
            )
    resolved = [path.resolve(strict=True) for path in inputs]
    resolved.extend(path.resolve(strict=False) for path in outputs)
    if len(set(resolved)) != len(resolved):
        raise EeaIedOverlayError("input and output paths must all be distinct")


def build(config: Config) -> dict[str, Any]:
    """Build the deterministic, create-only EEA blank-postcode overlay."""

    _validate_config(config)
    if shutil.disk_usage(config.output_csv.parent).free < config.minimum_free_bytes:
        raise EeaIedOverlayError("disk floor crossed before EEA overlay build")
    specs = (
        ("EEA source card", config.source.source_card, config.source.source_card_pin),
        ("EEA license freeze", config.source.license_freeze, config.source.license_freeze_pin),
        ("EEA snapshot", config.source.snapshot, config.source.snapshot_pin),
        (
            "EEA response headers",
            config.source.response_headers,
            config.source.response_headers_pin,
        ),
        (
            "EEA acquisition receipt",
            config.source.acquisition_receipt,
            config.source.acquisition_receipt_pin,
        ),
        ("builder CSV", config.builder_csv, config.builder_pin),
    )
    opened: list[tuple[str, builder.PinnedFile]] = []
    metadata_opened: list[tuple[str, builder.PinnedFile]] = []
    try:
        for label, path, pin in specs:
            opened.append((label, _open_pinned(path, pin, label)))
        files = dict(opened)
        files["EEA response headers"].stream.seek(0)
        headers = files["EEA response headers"].stream.read().decode(
            "iso-8859-1", errors="strict"
        )
        metadata_specs = _validate_source_documents(
            _read_json_object(files["EEA source card"], "EEA source card"),
            _read_json_object(files["EEA license freeze"], "EEA license freeze"),
            _read_json_object(
                files["EEA acquisition receipt"], "EEA acquisition receipt"
            ),
            headers,
            config.source,
            expected_source_records=config.expected_source_records,
        )
        primary_paths = {pinned.path.resolve(strict=True) for _, pinned in opened}
        metadata_paths: set[Path] = set()
        for label, path, pin in metadata_specs:
            resolved = path.resolve(strict=True)
            if resolved in primary_paths or resolved in metadata_paths:
                raise EeaIedOverlayError("EEA evidence path is reused")
            metadata_paths.add(resolved)
            metadata_opened.append((label, _open_pinned(path, pin, label)))
        contents: dict[str, bytes] = {}
        for label, pinned in metadata_opened:
            pinned.stream.seek(0)
            contents[label] = pinned.stream.read()
        files["EEA snapshot"].stream.seek(0)
        _validate_primary_contents(
            contents,
            canonical_snapshot=files["EEA snapshot"].stream.read(),
            expected_records=config.expected_source_records,
        )
        projections, source_counts = load_projections(
            files["EEA snapshot"], expected_records=config.expected_source_records
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
            raise EeaIedOverlayError("EEA overlay changed the builder row count")
        if counts.get("base_rows_retained") != config.expected_builder_rows:
            raise EeaIedOverlayError("EEA overlay dropped a base row")
        if counts.get("blank_postcode_rows_filled") != counts.get(
            "fillable_builder_rows"
        ):
            raise EeaIedOverlayError("EEA fill count changed between passes")
        if counts.get("blank_postcode_rows_filled", 0) <= 0:
            raise EeaIedOverlayError("EEA postcode overlay is vacuous")
        receipt: dict[str, Any] = {
            "schema": SCHEMA,
            "status": config.status,
            "license": {**_license_document(), "attribution": ATTRIBUTION},
            "policy": {
                "outcome_blind_full_safe_snapshot": True,
                "exact_source_fields": list(SAFE_SOURCE_FIELDS),
                "fixed_country_code": FIXED_COUNTRY_CODE,
                "fixed_reporting_year": FIXED_REPORTING_YEAR,
                "source_identifiers_names_coordinates_confidentiality_used": False,
                "strict_positive_single_house_suffix_only": True,
                "source_range_and_compound_house_veto": True,
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
                "single_house_grammar": _POSITIVE_SINGLE_HOUSE.pattern,
                "builder_header": list(builder.BUILDER_HEADER),
            },
            "inputs": {
                "source_card": dict(files["EEA source card"].evidence),
                "license_freeze": dict(files["EEA license freeze"].evidence),
                "snapshot": dict(files["EEA snapshot"].evidence),
                "response_headers": dict(files["EEA response headers"].evidence),
                "acquisition_receipt": dict(
                    files["EEA acquisition receipt"].evidence
                ),
                "evidence_chain": {
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
        snapshot=FROZEN_SNAPSHOT,
        snapshot_pin=FROZEN_SNAPSHOT_PIN,
        response_headers=FROZEN_RESPONSE_HEADERS,
        response_headers_pin=FROZEN_RESPONSE_HEADERS_PIN,
        acquisition_receipt=FROZEN_ACQUISITION_RECEIPT,
        acquisition_receipt_pin=FROZEN_ACQUISITION_RECEIPT_PIN,
        snapshot_date="2026-08-29",
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in (
        "builder-csv",
        "output-csv",
        "receipt",
    ):
        parser.add_argument(f"--{name}", type=Path, required=True)
    for name in (
        "builder-sha256",
    ):
        parser.add_argument(f"--{name}", required=True)
    for name in (
        "builder-bytes",
        "expected-builder-rows",
    ):
        parser.add_argument(f"--{name}", type=int, required=True)
    parser.add_argument("--minimum-free-bytes", type=int, default=DEFAULT_MIN_FREE_BYTES)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    def pin(name: str) -> Pin:
        return Pin(getattr(args, f"{name}_sha256"), getattr(args, f"{name}_bytes"))
    try:
        receipt = build(
            Config(
                source=frozen_source_contract(),
                expected_source_records=EXPECTED_SOURCE_RECORDS,
                builder_csv=args.builder_csv,
                builder_pin=pin("builder"),
                expected_builder_rows=args.expected_builder_rows,
                output_csv=args.output_csv,
                receipt=args.receipt,
                minimum_free_bytes=args.minimum_free_bytes,
            )
        )
    except EeaIedOverlayError as exc:
        raise SystemExit(f"error: {exc}") from exc
    print(json.dumps(receipt, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
