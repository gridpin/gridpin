#!/usr/bin/env python3
"""Fill blank DE builder postcodes from a closed, pinned MaStR aggregation.

This is deliberately a separate offline consumer of the SQLite evidence emitted
by :mod:`de_mastr_address_supplement`.  It does not acquire MaStR, read an
evaluation corpus, add address points, or read source coordinate columns.  The
producer's aggregate conflict bit may conservatively veto a candidate when its
source observations disagreed, including coordinate disagreement; coordinates
can never admit a candidate or enter the output.  A postcode may fill an
existing builder row only when all of these product-visible facts agree:

* the source street, single house, suffix and full postcode are exact;
* an exact source locality/municipality alias plus that postcode resolves to
  exactly one builder locality code;
* that exact street/house/suffix/code identity already exists in the builder;
* the identity has at least one blank postcode cell and no conflicting typed
  postcode; and
* every eligible source projection for the street/house/suffix surface reduces
  to the same code/postcode proposal.

Equal confirmations collapse.  Conflicting source projections, locality-code
ambiguity, postcode ambiguity, receipt drift, schema drift and resource-ceiling
violations all fail closed.  Output is row-for-row identical to the accepted
builder except for blank ``code_postal``/``code_postal_display`` pairs filled
with the admitted full postcode.  Outputs are write-once evidence: a late
failure intentionally leaves the unreceipted path occupied, and a retry must use
a new versioned path rather than deleting or overwriting it.
"""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
from collections.abc import Iterator, Mapping, Sequence
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import stat
from typing import Any
from urllib.parse import quote

import de_bnetza_address_supplement as bnetza
import de_photon_osm_supplement as builder


SCHEMA = "gridpin-de-mastr-postcode-fill-from-sqlite-v1"
SOURCE_RECEIPT_SCHEMA = "gridpin-de-mastr-address-supplement-v1"
STATUS = "PUBLIC_PERMISSIVE_DEVELOPMENT"
SOURCE_LICENSE = "DL-DE-BY-2.0"

DEFAULT_MIN_FREE_BYTES = 5 * 2**30
DEFAULT_MAX_OUTPUT_BYTES = 2 * 2**30
DEFAULT_MAX_CANDIDATE_ROWS = 5_000_000
DEFAULT_MAX_BASE_HIT_ROWS = 10_000_000
DEFAULT_MAX_RELATION_ROWS = 1_000_000
DEFAULT_MAX_SURFACE_ROWS = 100_000
DEFAULT_MAX_FILL_IDENTITIES = 1_000_000
DEFAULT_OUTPUT_CHECK_ROWS = 10_000

_SHA256 = re.compile(r"[0-9a-f]{64}")
_POSTCODE = re.compile(r"[0-9]{5}")
_MUNICIPALITY_KEY = re.compile(r"[0-9]{8}")


class MastrPostcodeFillError(RuntimeError):
    """The offline fill cannot continue without weakening a guard."""


@dataclass(frozen=True)
class Pin:
    sha256: str
    bytes: int


@dataclass(frozen=True)
class Config:
    builder_csv: Path
    builder_pin: Pin
    expected_builder_rows: int
    source_sqlite: Path
    sqlite_pin: Pin
    source_receipt: Path
    receipt_pin: Pin
    output_csv: Path | None = None
    output_receipt: Path | None = None
    expected_filled_rows: int | None = None
    minimum_free_bytes: int = DEFAULT_MIN_FREE_BYTES
    max_output_bytes: int = DEFAULT_MAX_OUTPUT_BYTES
    max_candidate_rows: int = DEFAULT_MAX_CANDIDATE_ROWS
    max_base_hit_rows: int = DEFAULT_MAX_BASE_HIT_ROWS
    max_relation_rows: int = DEFAULT_MAX_RELATION_ROWS
    max_surface_rows: int = DEFAULT_MAX_SURFACE_ROWS
    max_fill_identities: int = DEFAULT_MAX_FILL_IDENTITIES
    output_check_rows: int = DEFAULT_OUTPUT_CHECK_ROWS


@dataclass(frozen=True)
class Inputs:
    builder: builder.PinnedFile
    database: builder.PinnedFile
    receipt: builder.PinnedFile
    source_report: Mapping[str, Any]


@dataclass(frozen=True)
class Proposal:
    identity: tuple[str, str, int, str]
    postcode: str
    confirmations: int


# These are the exact six user tables created by ProjectionStore.  Whitespace
# is normalised before comparison, but table/column/constraint structure is not.
# Keeping the contract here prevents an old or repurposed SQLite file from being
# silently interpreted as source evidence.
DATABASE_SCHEMA_SQL: tuple[str, ...] = (
    """
    CREATE TABLE candidate (
        street_norm TEXT NOT NULL,
        number INTEGER NOT NULL,
        suffix TEXT NOT NULL,
        locality_norm TEXT NOT NULL,
        municipality_norm TEXT NOT NULL,
        municipality_key TEXT NOT NULL,
        postcode TEXT NOT NULL,
        lon TEXT NOT NULL,
        lat TEXT NOT NULL,
        street_display TEXT NOT NULL,
        locality_display TEXT NOT NULL,
        municipality_display TEXT NOT NULL,
        observations INTEGER NOT NULL,
        conflict INTEGER NOT NULL,
        PRIMARY KEY (
            street_norm, number, suffix, locality_norm,
            municipality_norm, municipality_key
        )
    ) WITHOUT ROWID
    """,
    """
    CREATE TABLE base_hit (
        street_norm TEXT NOT NULL,
        number INTEGER NOT NULL,
        suffix TEXT NOT NULL,
        code TEXT NOT NULL,
        postcode TEXT NOT NULL,
        PRIMARY KEY(street_norm, number, suffix, code, postcode)
    ) WITHOUT ROWID
    """,
    """
    CREATE TABLE locality (
        code TEXT NOT NULL PRIMARY KEY,
        locality_norm TEXT NOT NULL,
        locality_display TEXT NOT NULL,
        province TEXT NOT NULL
    ) WITHOUT ROWID
    """,
    """
    CREATE TABLE locality_relation (
        locality_norm TEXT NOT NULL,
        postcode TEXT NOT NULL,
        code TEXT NOT NULL,
        PRIMARY KEY(locality_norm, postcode, code)
    ) WITHOUT ROWID
    """,
    """
    CREATE TABLE processed_member (
        name TEXT NOT NULL PRIMARY KEY,
        ordinal INTEGER NOT NULL UNIQUE,
        evidence_json TEXT NOT NULL,
        counts_json TEXT NOT NULL
    ) WITHOUT ROWID
    """,
    """
    CREATE TABLE resolved (
        street_norm TEXT NOT NULL,
        code TEXT NOT NULL,
        number INTEGER NOT NULL,
        suffix TEXT NOT NULL,
        postcode TEXT NOT NULL,
        lon TEXT NOT NULL,
        lat TEXT NOT NULL,
        street_display TEXT NOT NULL,
        locality_norm TEXT NOT NULL,
        locality_display TEXT NOT NULL,
        province TEXT NOT NULL,
        municipality_key TEXT NOT NULL,
        observations INTEGER NOT NULL,
        conflict INTEGER NOT NULL,
        PRIMARY KEY(street_norm, code, number, suffix)
    ) WITHOUT ROWID
    """,
)

_CANDIDATE_SELECTION_SQL = """
    SELECT street_norm, number, suffix, locality_norm, municipality_norm,
           municipality_key, postcode, observations, conflict
    FROM candidate
    ORDER BY street_norm, number, suffix, locality_norm,
             municipality_norm, municipality_key
"""

_BASE_HIT_SELECTION_SQL = """
    SELECT street_norm, number, suffix, code, postcode
    FROM base_hit
    ORDER BY street_norm, number, suffix, code, postcode
"""


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
    if not isinstance(pin.sha256, str) or _SHA256.fullmatch(pin.sha256) is None:
        raise MastrPostcodeFillError(f"{label} SHA-256 pin is invalid")
    if isinstance(pin.bytes, bool) or not isinstance(pin.bytes, int) or pin.bytes <= 0:
        raise MastrPostcodeFillError(f"{label} byte pin must be positive")


def _open_pinned(path: Path, pin: Pin, label: str) -> builder.PinnedFile:
    _validate_pin(pin, label)
    try:
        pinned = builder._open_pinned(
            path,
            builder.Pin(sha256=pin.sha256, bytes=pin.bytes),
            label,
        )
    except builder.SupplementError as exc:
        raise MastrPostcodeFillError(str(exc)) from exc
    mode = stat.S_IMODE(pinned.stat_result.st_mode)
    if mode & 0o022:
        pinned.stream.close()
        raise MastrPostcodeFillError(
            f"pinned {label} must not be group/world writable: {mode:04o}"
        )
    return pinned


def _recheck_pinned(pinned: builder.PinnedFile, label: str) -> None:
    original_mode = stat.S_IMODE(pinned.stat_result.st_mode)
    try:
        builder._recheck_pinned(pinned, label)
        held = os.fstat(pinned.stream.fileno())
        named = os.stat(pinned.path, follow_symlinks=False)
    except (builder.SupplementError, OSError) as exc:
        raise MastrPostcodeFillError(str(exc)) from exc
    if stat.S_IMODE(held.st_mode) != original_mode or stat.S_IMODE(named.st_mode) != original_mode:
        raise MastrPostcodeFillError(f"pinned {label} mode changed during processing")


def _strict_json_from_pinned(pinned: builder.PinnedFile, label: str) -> Any:
    pinned.stream.seek(0)
    try:
        raw = pinned.stream.read().decode("utf-8", errors="strict")
        value = builder.strict_json_loads(raw)
    except (UnicodeDecodeError, ValueError, json.JSONDecodeError) as exc:
        raise MastrPostcodeFillError(f"{label} is not strict UTF-8 JSON: {exc}") from exc
    finally:
        pinned.stream.seek(0)
    return value


def _positive_int(value: Any, label: str, *, allow_zero: bool = False) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise MastrPostcodeFillError(f"{label} must be an integer")
    if value < 0 or (not allow_zero and value == 0):
        raise MastrPostcodeFillError(f"{label} is outside its allowed range")
    return value


def _mapping(value: Any, label: str) -> Mapping[str, Any]:
    if not isinstance(value, dict):
        raise MastrPostcodeFillError(f"{label} must be an object")
    return value


def _receipt_evidence(
    value: Any,
    pin: Pin,
    label: str,
) -> Mapping[str, Any]:
    evidence = _mapping(value, label)
    if evidence.get("sha256") != pin.sha256 or evidence.get("bytes") != pin.bytes:
        raise MastrPostcodeFillError(f"source receipt does not bind the pinned {label}")
    path_value = evidence.get("path")
    if not isinstance(path_value, str) or not path_value:
        raise MastrPostcodeFillError(f"source receipt {label} path is missing")
    return evidence


def _validate_source_receipt(
    value: Any,
    config: Config,
) -> Mapping[str, Any]:
    report = _mapping(value, "source receipt")
    if report.get("schema") != SOURCE_RECEIPT_SCHEMA:
        raise MastrPostcodeFillError("source receipt schema drift")
    if report.get("status") != STATUS:
        raise MastrPostcodeFillError("source receipt status drift")
    source = _mapping(report.get("source"), "source receipt source")
    if source.get("license") != SOURCE_LICENSE:
        raise MastrPostcodeFillError("source receipt license drift")
    policy = _mapping(report.get("policy"), "source receipt policy")
    required_policy = {
        "sqlite_candidate_and_base_relations_reusable_offline": True,
        "strict_public_address_flags": True,
        "checked_active_in_operation_only": True,
        "complete_product_locality_relation": True,
        "equal_observations_collapsed": True,
        "conflicting_projection_fail_closed": True,
        "base_rows_order_and_coordinates_preserved": True,
        "source_only_additions": True,
        "postcode_fill_applied": False,
        "first_member_selection": False,
        "evaluation_inputs_read": False,
    }
    if any(policy.get(key) is not expected for key, expected in required_policy.items()):
        raise MastrPostcodeFillError("source receipt policy is not reusable for offline fill")
    inputs = _mapping(report.get("inputs"), "source receipt inputs")
    _receipt_evidence(inputs.get("builder"), config.builder_pin, "builder")
    _receipt_evidence(report.get("aggregation"), config.sqlite_pin, "aggregation")
    counts = _mapping(report.get("counts"), "source receipt counts")
    candidates = _positive_int(
        counts.get("unique_semantic_candidates"),
        "source receipt unique candidate count",
    )
    members = _positive_int(
        counts.get("address_members_processed"),
        "source receipt processed member count",
    )
    base_rows = _positive_int(
        counts.get("base_rows_scanned"),
        "source receipt builder row count",
    )
    if base_rows != config.expected_builder_rows:
        raise MastrPostcodeFillError("source receipt builder row-count binding drift")
    if candidates > config.max_candidate_rows:
        raise MastrPostcodeFillError("source receipt candidate ceiling crossed")
    if members > 128:
        raise MastrPostcodeFillError("source receipt member ceiling crossed")
    return report


def _validate_config(config: Config, *, for_build: bool) -> None:
    for label, pin in (
        ("builder", config.builder_pin),
        ("SQLite", config.sqlite_pin),
        ("source receipt", config.receipt_pin),
    ):
        _validate_pin(pin, label)
    if config.builder_csv.suffix.lower() != ".gz":
        raise MastrPostcodeFillError("accepted builder must be gzip")
    if config.expected_builder_rows <= 0:
        raise MastrPostcodeFillError("expected builder rows must be positive")
    if config.expected_filled_rows is not None and config.expected_filled_rows <= 0:
        raise MastrPostcodeFillError("expected filled rows must be positive when pinned")
    for value, label in (
        (config.minimum_free_bytes, "minimum free bytes"),
        (config.max_output_bytes, "output bytes"),
        (config.max_candidate_rows, "candidate rows"),
        (config.max_base_hit_rows, "base-hit rows"),
        (config.max_relation_rows, "relation rows"),
        (config.max_surface_rows, "surface rows"),
        (config.max_fill_identities, "fill identities"),
        (config.output_check_rows, "output check rows"),
    ):
        if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
            if (
                label == "minimum free bytes"
                and not isinstance(value, bool)
                and value == 0
            ):
                continue
            raise MastrPostcodeFillError(f"maximum {label} must be positive")
    if for_build:
        if config.expected_filled_rows is None:
            raise MastrPostcodeFillError(
                "build requires expected filled rows from a prior pinned audit"
            )
        if config.output_csv is None or config.output_receipt is None:
            raise MastrPostcodeFillError("build requires output CSV and receipt")
        for path in (config.output_csv, config.output_receipt):
            if path.exists():
                raise MastrPostcodeFillError(f"output must not exist: {path}")
            if not path.parent.is_dir():
                raise MastrPostcodeFillError(f"output parent is missing: {path.parent}")


def _open_inputs(config: Config) -> Inputs:
    builder_file = _open_pinned(config.builder_csv, config.builder_pin, "accepted builder")
    database: builder.PinnedFile | None = None
    receipt: builder.PinnedFile | None = None
    try:
        database = _open_pinned(config.source_sqlite, config.sqlite_pin, "MaStR SQLite")
        receipt = _open_pinned(config.source_receipt, config.receipt_pin, "MaStR receipt")
        report = _validate_source_receipt(
            _strict_json_from_pinned(receipt, "MaStR receipt"),
            config,
        )
        return Inputs(builder_file, database, receipt, report)
    except BaseException:
        builder_file.stream.close()
        if database is not None:
            database.stream.close()
        if receipt is not None:
            receipt.stream.close()
        raise


def _normalised_sql(value: str) -> str:
    return re.sub(r"\s+", " ", value.strip()).lower()


def _validate_database_schema(connection: sqlite3.Connection) -> None:
    rows = connection.execute(
        """
        SELECT type, name, sql FROM sqlite_schema
        WHERE name NOT LIKE 'sqlite_%'
        ORDER BY type, name
        """
    ).fetchall()
    if any(kind != "table" or not isinstance(sql, str) for kind, _name, sql in rows):
        raise MastrPostcodeFillError("MaStR SQLite contains unexpected schema objects")
    observed = {_normalised_sql(sql) for _kind, _name, sql in rows}
    expected = {_normalised_sql(sql) for sql in DATABASE_SCHEMA_SQL}
    if observed != expected or len(rows) != len(DATABASE_SCHEMA_SQL):
        raise MastrPostcodeFillError("MaStR SQLite exact schema drift")
    page_size = int(connection.execute("PRAGMA page_size").fetchone()[0])
    user_version = int(connection.execute("PRAGMA user_version").fetchone()[0])
    application_id = int(connection.execute("PRAGMA application_id").fetchone()[0])
    if (page_size, user_version, application_id) != (4096, 0, 0):
        raise MastrPostcodeFillError("MaStR SQLite file-header contract drift")
    integrity = connection.execute("PRAGMA integrity_check").fetchall()
    if integrity != [("ok",)]:
        raise MastrPostcodeFillError("MaStR SQLite integrity check failed")
    if connection.execute("PRAGMA foreign_key_check").fetchone() is not None:
        raise MastrPostcodeFillError("MaStR SQLite foreign-key check failed")


def _open_database(pinned: builder.PinnedFile) -> sqlite3.Connection:
    absolute = os.path.abspath(os.fspath(pinned.path))
    uri = "file:" + quote(absolute, safe="/") + "?mode=ro&immutable=1"
    connection: sqlite3.Connection | None = None
    try:
        connection = sqlite3.connect(uri, uri=True)
        connection.execute("PRAGMA query_only=ON")
        connection.execute("PRAGMA temp_store=MEMORY")
        if int(connection.execute("PRAGMA query_only").fetchone()[0]) != 1:
            raise MastrPostcodeFillError("SQLite query-only mode was not applied")
        _validate_database_schema(connection)
        return connection
    except (sqlite3.Error, OSError) as exc:
        if connection is not None:
            connection.close()
        raise MastrPostcodeFillError(f"cannot open immutable MaStR SQLite: {exc}") from exc
    except BaseException:
        if connection is not None:
            connection.close()
        raise


def _bounded_count(
    connection: sqlite3.Connection,
    table: str,
    ceiling: int,
) -> int:
    if table not in {"candidate", "base_hit", "locality_relation", "processed_member"}:
        raise MastrPostcodeFillError("internal table allowlist failure")
    value = int(connection.execute(f"SELECT count(*) FROM {table}").fetchone()[0])
    if value < 0 or value > ceiling:
        raise MastrPostcodeFillError(f"{table} row ceiling crossed: {value} > {ceiling}")
    return value


def _load_relations(
    connection: sqlite3.Connection,
    *,
    expected_rows: int,
) -> dict[tuple[str, str], frozenset[str]]:
    values: defaultdict[tuple[str, str], set[str]] = defaultdict(set)
    rows = 0
    previous: tuple[str, str, str] | None = None
    for locality, postcode, code in connection.execute(
        """
        SELECT locality_norm, postcode, code FROM locality_relation
        ORDER BY locality_norm, postcode, code
        """
    ):
        rows += 1
        current = (locality, postcode, code)
        if previous is not None and current <= previous:
            raise MastrPostcodeFillError("locality relation order/uniqueness drift")
        previous = current
        if (
            not isinstance(locality, str)
            or not locality
            or builder.normalize_text(locality) != locality
            or not isinstance(postcode, str)
            or _POSTCODE.fullmatch(postcode) is None
            or not isinstance(code, str)
            or not code
        ):
            raise MastrPostcodeFillError("malformed exact locality/postcode relation")
        values[(locality, postcode)].add(code)
    if rows != expected_rows:
        raise MastrPostcodeFillError("locality relation count drift during scan")
    return {key: frozenset(codes) for key, codes in values.items()}


def _surface(value: Sequence[Any]) -> tuple[str, int, str]:
    street, number, suffix = value[:3]
    if (
        not isinstance(street, str)
        or not street
        or builder.normalize_text(street) != street
        or isinstance(number, bool)
        or not isinstance(number, int)
        or number <= 0
        or number > 0xFFFF_FFFF
        or not isinstance(suffix, str)
        or builder.normalize_rep(suffix) != suffix
    ):
        raise MastrPostcodeFillError("malformed exact street/house/suffix surface")
    return street, number, suffix


def _grouped_rows(
    rows: Iterator[tuple[Any, ...]],
    *,
    max_group_rows: int,
    label: str,
) -> Iterator[tuple[tuple[str, int, str], list[tuple[Any, ...]]]]:
    current: tuple[str, int, str] | None = None
    group: list[tuple[Any, ...]] = []
    previous_row: tuple[Any, ...] | None = None
    for row in rows:
        surface = _surface(row)
        if previous_row is not None and row <= previous_row:
            raise MastrPostcodeFillError(f"{label} order/uniqueness drift")
        previous_row = row
        if current is not None and surface != current:
            yield current, group
            group = []
        current = surface
        group.append(row)
        if len(group) > max_group_rows:
            raise MastrPostcodeFillError(f"{label} surface ceiling crossed")
    if current is not None:
        yield current, group


def _validate_candidate(row: Sequence[Any]) -> tuple[str, str, str, str, int, int]:
    (
        _street,
        _number,
        _suffix,
        locality,
        municipality,
        municipality_key,
        postcode,
        observations,
        conflict,
    ) = row
    for value, label in ((locality, "locality"), (municipality, "municipality")):
        if (
            not isinstance(value, str)
            or not value
            or builder.normalize_text(value) != value
        ):
            raise MastrPostcodeFillError(f"malformed source {label}")
    if not isinstance(municipality_key, str) or _MUNICIPALITY_KEY.fullmatch(municipality_key) is None:
        raise MastrPostcodeFillError("malformed source municipality key")
    if not isinstance(postcode, str) or _POSTCODE.fullmatch(postcode) is None:
        raise MastrPostcodeFillError("malformed source full postcode")
    observations = _positive_int(observations, "source observations")
    if conflict not in (0, 1) or isinstance(conflict, bool):
        raise MastrPostcodeFillError("malformed source conflict flag")
    return locality, municipality, municipality_key, postcode, observations, conflict


def _validate_base_hits(rows: Sequence[Sequence[Any]]) -> dict[str, set[str]]:
    by_code: defaultdict[str, set[str]] = defaultdict(set)
    for row in rows:
        _street, _number, _suffix, code, postcode = row
        if not isinstance(code, str) or not code:
            raise MastrPostcodeFillError("malformed base-hit locality code")
        if not isinstance(postcode, str) or (
            postcode and _POSTCODE.fullmatch(postcode) is None
        ):
            raise MastrPostcodeFillError("malformed base-hit postcode")
        by_code[code].add(postcode)
    return dict(by_code)


def _resolve_surface(
    surface: tuple[str, int, str],
    candidates: Sequence[tuple[Any, ...]],
    base_hits: Sequence[tuple[Any, ...]],
    relations: Mapping[tuple[str, str], frozenset[str]],
    counts: Counter[str],
) -> Proposal | None:
    by_code = _validate_base_hits(base_hits)
    proposals: Counter[tuple[str, str]] = Counter()
    poisoned = False
    for candidate in candidates:
        locality, municipality, _key, postcode, observations, conflict = (
            _validate_candidate(candidate)
        )
        counts["source_candidate_rows_scanned"] += 1
        counts["source_observations_scanned"] += observations
        if conflict:
            counts["excluded_conflicting_source_projection"] += 1
            poisoned = True
            continue
        aliases = {locality, municipality}
        codes: set[str] = set()
        for alias in aliases:
            codes.update(relations.get((alias, postcode), ()))
        if not codes:
            counts["excluded_no_exact_alias_postcode_relation"] += 1
            continue
        if len(codes) != 1:
            counts["excluded_ambiguous_exact_alias_postcode_relation"] += 1
            poisoned = True
            continue
        code = next(iter(codes))
        identity_postcodes = by_code.get(code)
        if identity_postcodes is None:
            counts["excluded_no_exact_existing_identity"] += 1
            continue
        if "" not in identity_postcodes:
            counts["excluded_identity_without_blank_postcode"] += 1
            continue
        typed = identity_postcodes - {""}
        if typed and typed != {postcode}:
            counts["excluded_typed_postcode_conflict"] += 1
            poisoned = True
            continue
        proposals[(code, postcode)] += observations
    if poisoned:
        counts["excluded_poisoned_surface"] += 1
        return None
    if not proposals:
        return None
    if len(proposals) != 1:
        codes = {code for code, _postcode in proposals}
        postcodes = {postcode for _code, postcode in proposals}
        if len(codes) > 1:
            counts["excluded_conflicting_locality_code_surface"] += 1
        if len(postcodes) > 1:
            counts["excluded_semantic_postcode_ambiguity"] += 1
        counts["excluded_ambiguous_surface"] += 1
        return None
    (code, postcode), confirmations = next(iter(proposals.items()))
    counts["duplicate_confirmations_collapsed"] += confirmations - 1
    counts["fill_identity_proposals"] += 1
    return Proposal((*surface[:1], code, *surface[1:]), postcode, confirmations)


def _derive_proposals(
    connection: sqlite3.Connection,
    report: Mapping[str, Any],
    config: Config,
) -> tuple[dict[tuple[str, str, int, str], Proposal], Counter[str]]:
    counts: Counter[str] = Counter()
    candidate_rows = _bounded_count(
        connection, "candidate", config.max_candidate_rows
    )
    base_hit_rows = _bounded_count(
        connection, "base_hit", config.max_base_hit_rows
    )
    relation_rows = _bounded_count(
        connection, "locality_relation", config.max_relation_rows
    )
    member_rows = _bounded_count(connection, "processed_member", 128)
    source_counts = _mapping(report.get("counts"), "source receipt counts")
    if candidate_rows != source_counts.get("unique_semantic_candidates"):
        raise MastrPostcodeFillError("candidate count does not match source receipt")
    if member_rows != source_counts.get("address_members_processed"):
        raise MastrPostcodeFillError("processed-member count does not match source receipt")
    if candidate_rows <= 0 or base_hit_rows <= 0 or relation_rows <= 0:
        raise MastrPostcodeFillError("offline MaStR postcode evidence is vacuous")
    counts.update(
        {
            "database_candidate_rows": candidate_rows,
            "database_base_hit_rows": base_hit_rows,
            "database_locality_relation_rows": relation_rows,
            "database_processed_member_rows": member_rows,
        }
    )
    relations = _load_relations(connection, expected_rows=relation_rows)
    candidate_groups = iter(
        _grouped_rows(
            iter(connection.execute(_CANDIDATE_SELECTION_SQL)),
            max_group_rows=config.max_surface_rows,
            label="candidate",
        )
    )
    base_groups = iter(
        _grouped_rows(
            iter(connection.execute(_BASE_HIT_SELECTION_SQL)),
            max_group_rows=config.max_surface_rows,
            label="base-hit",
        )
    )
    candidate_group = next(candidate_groups, None)
    base_group = next(base_groups, None)
    proposals: dict[tuple[str, str, int, str], Proposal] = {}
    scanned_candidates = 0
    scanned_base_hits = 0
    while candidate_group is not None or base_group is not None:
        if candidate_group is None:
            raise MastrPostcodeFillError("base-hit surface has no source candidate")
        if base_group is None or candidate_group[0] < base_group[0]:
            scanned_candidates += len(candidate_group[1])
            for row in candidate_group[1]:
                _validate_candidate(row)
            counts["source_surfaces_without_base_hit"] += 1
            candidate_group = next(candidate_groups, None)
            continue
        if base_group[0] < candidate_group[0]:
            raise MastrPostcodeFillError("base-hit surface has no source candidate")
        surface = candidate_group[0]
        scanned_candidates += len(candidate_group[1])
        scanned_base_hits += len(base_group[1])
        proposal = _resolve_surface(
            surface,
            candidate_group[1],
            base_group[1],
            relations,
            counts,
        )
        if proposal is not None:
            old = proposals.get(proposal.identity)
            if old is not None and old.postcode != proposal.postcode:
                raise MastrPostcodeFillError("identity received two postcodes")
            proposals[proposal.identity] = proposal
            if len(proposals) > config.max_fill_identities:
                raise MastrPostcodeFillError("fill identity ceiling crossed")
        candidate_group = next(candidate_groups, None)
        base_group = next(base_groups, None)
    if scanned_candidates != candidate_rows or scanned_base_hits != base_hit_rows:
        raise MastrPostcodeFillError("candidate/base-hit scan conservation failed")
    if counts["source_candidate_rows_scanned"] > scanned_candidates:
        raise MastrPostcodeFillError("candidate resolution accounting overflow")
    counts["source_candidate_rows_scanned_total"] = scanned_candidates
    counts["base_hit_rows_scanned"] = scanned_base_hits
    counts["fill_identities"] = len(proposals)
    if not proposals:
        raise MastrPostcodeFillError("offline MaStR postcode fill is vacuous")
    return proposals, counts


def _raw_postcode(row: Mapping[str, str], row_number: int) -> str:
    numeric = row["code_postal"].strip()
    display = row["code_postal_display"].strip()
    if (numeric, display) == ("", ""):
        return ""
    if numeric == display and _POSTCODE.fullmatch(numeric) is not None:
        return numeric
    raise MastrPostcodeFillError(
        f"accepted builder has malformed raw postcode pair at row {row_number}"
    )


class GuardedWriter:
    def __init__(self, writer: Any, path: Path, config: Config) -> None:
        self.writer = writer
        self.path = path
        self.max_bytes = config.max_output_bytes
        self.minimum_free_bytes = config.minimum_free_bytes
        self.check_every = config.output_check_rows
        self.rows = 0

    def check(self) -> None:
        try:
            size = self.path.stat().st_size
            free = shutil.disk_usage(self.path.parent).free
        except OSError as exc:
            raise MastrPostcodeFillError("cannot inspect guarded output") from exc
        if size > self.max_bytes:
            raise MastrPostcodeFillError("output byte ceiling crossed")
        if free < self.minimum_free_bytes:
            raise MastrPostcodeFillError("disk floor crossed while writing output")

    def writerow(self, row: Mapping[str, str]) -> None:
        self.writer.writerow(row)
        self.rows += 1
        if self.rows % self.check_every == 0:
            self.check()


def _scan_or_write_builder(
    pinned: builder.PinnedFile,
    proposals: Mapping[tuple[str, str, int, str], Proposal],
    config: Config,
    writer: GuardedWriter | None,
) -> Counter[str]:
    counts: Counter[str] = Counter()
    used: set[tuple[str, str, int, str]] = set()
    row_number = 0
    try:
        groups = bnetza._iter_builder_groups(pinned)
        for identity, rows in groups:
            proposal = proposals.get(identity)
            postcodes: list[str] = []
            for offset, row in enumerate(rows, 1):
                postcodes.append(_raw_postcode(row, row_number + offset))
            if proposal is not None:
                typed = set(postcodes) - {""}
                if typed and typed != {proposal.postcode}:
                    raise MastrPostcodeFillError(
                        "accepted builder typed postcode conflicts with proposal"
                    )
                if "" not in postcodes:
                    raise MastrPostcodeFillError(
                        "postcode proposal does not target a blank builder row"
                    )
                used.add(identity)
            for row, postcode in zip(rows, postcodes):
                row_number += 1
                if proposal is not None and not postcode:
                    if writer is not None:
                        projected = dict(row)
                        projected["code_postal"] = proposal.postcode
                        projected["code_postal_display"] = proposal.postcode
                        writer.writerow(projected)
                    counts["builder_blank_postcode_rows_filled"] += 1
                else:
                    if writer is not None:
                        writer.writerow(row)
                    if postcode:
                        counts["builder_nonblank_postcode_rows_unchanged"] += 1
                    else:
                        counts["builder_blank_postcode_rows_unchanged"] += 1
                counts["builder_rows_retained"] += 1
                counts["output_rows"] += 1
    except bnetza.BNetzASupplementError as exc:
        raise MastrPostcodeFillError(str(exc)) from exc
    if counts["builder_rows_retained"] != config.expected_builder_rows:
        raise MastrPostcodeFillError("builder/output row-count conservation failed")
    if counts["output_rows"] != config.expected_builder_rows:
        raise MastrPostcodeFillError("output row-count conservation failed")
    if used != set(proposals):
        raise MastrPostcodeFillError("database proposal did not match the pinned builder")
    if counts["builder_blank_postcode_rows_filled"] <= 0:
        raise MastrPostcodeFillError("offline MaStR postcode output is vacuous")
    return counts


def _output_plan(counts: Mapping[str, int]) -> dict[str, int]:
    return {
        "builder_blank_postcode_rows_to_fill": counts[
            "builder_blank_postcode_rows_filled"
        ],
        "builder_blank_postcode_rows_unchanged": counts[
            "builder_blank_postcode_rows_unchanged"
        ],
        "builder_nonblank_postcode_rows_unchanged": counts[
            "builder_nonblank_postcode_rows_unchanged"
        ],
        "builder_rows_retained": counts["builder_rows_retained"],
        "output_rows": counts["output_rows"],
    }


def _write_output(
    pinned: builder.PinnedFile,
    proposals: Mapping[tuple[str, str, int, str], Proposal],
    config: Config,
) -> Counter[str]:
    assert config.output_csv is not None
    with builder._canonical_gzip_writer(
        config.output_csv, builder.BUILDER_HEADER
    ) as csv_writer:
        writer = GuardedWriter(csv_writer, config.output_csv, config)
        counts = _scan_or_write_builder(pinned, proposals, config, writer)
        if counts["builder_blank_postcode_rows_filled"] != config.expected_filled_rows:
            raise MastrPostcodeFillError("filled-row count pin mismatch")
        writer.check()
    # text.flush(), gzip.close() and fsync happen only while leaving the
    # context above.  Recheck the final compressed bytes/footer and disk
    # floor, not merely the still-buffered payload observed inside it.
    writer.check()
    return counts


def _file_evidence(path: Path) -> dict[str, Any]:
    digest = hashlib.sha256()
    size = 0
    try:
        with path.open("rb") as handle:
            info = os.fstat(handle.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                raise MastrPostcodeFillError("output must be a regular nlink=1 file")
            for block in iter(lambda: handle.read(8 * 2**20), b""):
                digest.update(block)
                size += len(block)
    except OSError as exc:
        raise MastrPostcodeFillError(f"cannot read output evidence: {path}") from exc
    return {
        "path": str(path),
        "bytes": size,
        "sha256": digest.hexdigest(),
        "mode": f"{stat.S_IMODE(info.st_mode):04o}",
        "nlink": info.st_nlink,
    }


def _audit_internal(config: Config) -> tuple[Inputs, dict[tuple[str, str, int, str], Proposal], dict[str, Any]]:
    _validate_config(config, for_build=False)
    inputs = _open_inputs(config)
    connection: sqlite3.Connection | None = None
    try:
        connection = _open_database(inputs.database)
        data_version = int(connection.execute("PRAGMA data_version").fetchone()[0])
        proposals, counts = _derive_proposals(
            connection, inputs.source_report, config
        )
        if int(connection.execute("PRAGMA data_version").fetchone()[0]) != data_version:
            raise MastrPostcodeFillError("immutable SQLite data version changed")
        inputs.builder.stream.seek(0)
        planned_output_counts = _scan_or_write_builder(
            inputs.builder, proposals, config, None
        )
        _recheck_pinned(inputs.builder, "accepted builder")
        _recheck_pinned(inputs.database, "MaStR SQLite")
        _recheck_pinned(inputs.receipt, "MaStR receipt")
        report: dict[str, Any] = {
            "schema": SCHEMA,
            "status": STATUS,
            "policy": {
                "offline_only": True,
                "sqlite_open_mode": "ro+immutable+query_only",
                "source_receipt_schema": SOURCE_RECEIPT_SCHEMA,
                "exact_database_schema_required": True,
                "source_coordinates_selected": False,
                "source_coordinates_written": False,
                "source_coordinate_conflict_may_only_veto": True,
                "existing_street_house_suffix_code_identity_required": True,
                "exact_alias_and_full_postcode_relation_required": True,
                "blank_postcode_cells_only": True,
                "typed_postcode_conflict_fail_closed": True,
                "semantic_postcode_or_code_ambiguity_fail_closed": True,
                "duplicate_confirmations_collapsed": True,
                "first_candidate_selection": False,
                "base_rows_order_and_coordinates_preserved": True,
                "source_rows_added": False,
                "evaluation_inputs_read": False,
                "network_calls": 0,
                "gridpin_engine_calls": 0,
                "photon_engine_calls": 0,
            },
            "limits": {
                "minimum_free_bytes": config.minimum_free_bytes,
                "max_output_bytes": config.max_output_bytes,
                "max_candidate_rows": config.max_candidate_rows,
                "max_base_hit_rows": config.max_base_hit_rows,
                "max_relation_rows": config.max_relation_rows,
                "max_surface_rows": config.max_surface_rows,
                "max_fill_identities": config.max_fill_identities,
                "output_check_rows": config.output_check_rows,
            },
            "configuration": {
                "expected_builder_rows": config.expected_builder_rows,
                "expected_filled_rows": config.expected_filled_rows,
            },
            "inputs": {
                "accepted_builder": dict(inputs.builder.evidence),
                "source_sqlite": dict(inputs.database.evidence),
                "source_receipt": dict(inputs.receipt.evidence),
            },
            "source_binding": {
                "schema": inputs.source_report["schema"],
                "status": inputs.source_report["status"],
                "license": inputs.source_report["source"]["license"],
                "builder_sha256": config.builder_pin.sha256,
                "sqlite_sha256": config.sqlite_pin.sha256,
            },
            "output_plan": _output_plan(planned_output_counts),
            "counts": dict(sorted(counts.items())),
        }
        return inputs, proposals, report
    except BaseException:
        inputs.builder.stream.close()
        inputs.database.stream.close()
        inputs.receipt.stream.close()
        raise
    finally:
        if connection is not None:
            connection.close()


def audit(config: Config) -> dict[str, Any]:
    """Validate all closed inputs and derive the non-vacuous fill set."""

    inputs, _proposals, report = _audit_internal(config)
    inputs.builder.stream.close()
    inputs.database.stream.close()
    inputs.receipt.stream.close()
    return report


def build(config: Config) -> dict[str, Any]:
    """Write the row-preserving postcode-fill builder and write-once receipt."""

    _validate_config(config, for_build=True)
    assert config.output_csv is not None
    assert config.output_receipt is not None
    free = min(
        shutil.disk_usage(path.parent).free
        for path in (config.output_csv, config.output_receipt)
    )
    if free < config.minimum_free_bytes:
        raise MastrPostcodeFillError(
            f"disk floor crossed before offline fill: {free} < {config.minimum_free_bytes}"
        )
    inputs, proposals, report = _audit_internal(config)
    try:
        planned_filled_rows = report["output_plan"][
            "builder_blank_postcode_rows_to_fill"
        ]
        if planned_filled_rows != config.expected_filled_rows:
            raise MastrPostcodeFillError("filled-row count pin mismatch")
        inputs.builder.stream.seek(0)
        output_counts = _write_output(inputs.builder, proposals, config)
        if _output_plan(output_counts) != report["output_plan"]:
            raise MastrPostcodeFillError("builder output plan changed between passes")
        _recheck_pinned(inputs.builder, "accepted builder")
        _recheck_pinned(inputs.database, "MaStR SQLite")
        _recheck_pinned(inputs.receipt, "MaStR receipt")
        counts = Counter(report["counts"])
        counts.update(output_counts)
        if counts["builder_rows_retained"] != config.expected_builder_rows:
            raise MastrPostcodeFillError("final row conservation failed")
        report["counts"] = dict(sorted(counts.items()))
        report["output"] = _file_evidence(config.output_csv)
        with config.output_receipt.open("xb") as handle:
            handle.write(canonical_json_bytes(report))
            handle.flush()
            os.fsync(handle.fileno())
        return report
    finally:
        inputs.builder.stream.close()
        inputs.database.stream.close()
        inputs.receipt.stream.close()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audit-only", action="store_true")
    parser.add_argument("--builder", type=Path, required=True)
    parser.add_argument("--builder-sha256", required=True)
    parser.add_argument("--builder-bytes", type=int, required=True)
    parser.add_argument("--expected-builder-rows", type=int, required=True)
    parser.add_argument("--source-sqlite", type=Path, required=True)
    parser.add_argument("--sqlite-sha256", required=True)
    parser.add_argument("--sqlite-bytes", type=int, required=True)
    parser.add_argument("--source-receipt", type=Path, required=True)
    parser.add_argument("--receipt-sha256", required=True)
    parser.add_argument("--receipt-bytes", type=int, required=True)
    parser.add_argument("--output-csv", type=Path)
    parser.add_argument("--output-receipt", type=Path)
    parser.add_argument("--expected-filled-rows", type=int)
    parser.add_argument("--minimum-free-bytes", type=int, default=DEFAULT_MIN_FREE_BYTES)
    parser.add_argument("--max-output-bytes", type=int, default=DEFAULT_MAX_OUTPUT_BYTES)
    parser.add_argument("--max-candidate-rows", type=int, default=DEFAULT_MAX_CANDIDATE_ROWS)
    parser.add_argument("--max-base-hit-rows", type=int, default=DEFAULT_MAX_BASE_HIT_ROWS)
    parser.add_argument("--max-relation-rows", type=int, default=DEFAULT_MAX_RELATION_ROWS)
    parser.add_argument("--max-surface-rows", type=int, default=DEFAULT_MAX_SURFACE_ROWS)
    parser.add_argument("--max-fill-identities", type=int, default=DEFAULT_MAX_FILL_IDENTITIES)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    if args.audit_only and (args.output_csv is not None or args.output_receipt is not None):
        raise SystemExit("audit-only cannot create outputs")
    if not args.audit_only and (
        args.output_csv is None or args.output_receipt is None
    ):
        raise SystemExit("build requires --output-csv and --output-receipt")
    config = Config(
        builder_csv=args.builder,
        builder_pin=Pin(args.builder_sha256, args.builder_bytes),
        expected_builder_rows=args.expected_builder_rows,
        source_sqlite=args.source_sqlite,
        sqlite_pin=Pin(args.sqlite_sha256, args.sqlite_bytes),
        source_receipt=args.source_receipt,
        receipt_pin=Pin(args.receipt_sha256, args.receipt_bytes),
        output_csv=args.output_csv,
        output_receipt=args.output_receipt,
        expected_filled_rows=args.expected_filled_rows,
        minimum_free_bytes=args.minimum_free_bytes,
        max_output_bytes=args.max_output_bytes,
        max_candidate_rows=args.max_candidate_rows,
        max_base_hit_rows=args.max_base_hit_rows,
        max_relation_rows=args.max_relation_rows,
        max_surface_rows=args.max_surface_rows,
        max_fill_identities=args.max_fill_identities,
    )
    try:
        report = audit(config) if args.audit_only else build(config)
    except MastrPostcodeFillError as exc:
        raise SystemExit(f"DE MaStR offline postcode fill refused: {exc}") from exc
    print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
