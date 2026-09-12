#!/usr/bin/env python3
"""Build a strict source-only DE address supplement from a pinned MaStR ZIP.

The acquisition path is deliberately seek-free and bounded.  A pinned JSON
manifest describes the ZIP central directory and every local member range.
Selected public ``Einheiten*.xml`` members are fetched one at a time with an
exact HTTP 206 contract, inflated incrementally, and parsed with
``XMLPullParser``.  The multi-gigabyte archive and its much larger expanded XML
are never written to disk.

Only public, checked, active, in-operation German units with a complete BKG-
recognised address and public coordinates may produce a candidate.  SQLite
provides a bounded aggregation surface.  Repeated equal observations collapse;
postcode, locality or coordinate disagreement fails closed.  Existing builder
identities are never filled or replaced: admitted rows are source-only additions.
"""

from __future__ import annotations

import argparse
from collections import Counter
from collections.abc import Iterator, Mapping, Sequence
from contextlib import contextmanager
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import struct
from typing import Any, BinaryIO, Callable, ContextManager, Protocol
import urllib.request
import xml.etree.ElementTree as ET
import zlib

import de_bnetza_address_supplement as bnetza
import de_photon_osm_supplement as builder


SCHEMA = "gridpin-de-mastr-address-supplement-v1"
MANIFEST_SCHEMA = "gridpin-de-mastr-zip-member-manifest-v1"
STATUS = "PUBLIC_PERMISSIVE_DEVELOPMENT"
LICENSE = "DL-DE-BY-2.0"
LICENSE_NAME = "Datenlizenz Deutschland - Namensnennung - Version 2.0"
LICENSE_URL = "https://www.govdata.de/dl-de/by-2-0"
ATTRIBUTION = "Bundesnetzagentur, Marktstammdatenregister (MaStR)"

OFFICIAL_SOURCE_URL = (
    "https://download.marktstammdatenregister.de/"
    "Gesamtdatenexport_20260829_26.1.zip"
)
OFFICIAL_CONTENT_LENGTH = 3_163_846_118
OFFICIAL_ETAG = '"c9edf0d35537dd1:0"'
OFFICIAL_LAST_MODIFIED = "Sat, 29 Aug 2026 01:29:25 GMT"
OFFICIAL_CONTENT_TYPE = "application/x-zip-compressed"
OFFICIAL_CENTRAL_DIRECTORY_OFFSET = 3_163_800_743
OFFICIAL_CENTRAL_DIRECTORY_BYTES = 45_353
OFFICIAL_CENTRAL_DIRECTORY_SHA256 = (
    "9c39a17884521e5cad526e0501412b784badac14f04f881769bde2ea2b0dc773"
)
OFFICIAL_EOCD_OFFSET = 3_163_846_096
OFFICIAL_ZIP_ENTRIES = 434
OFFICIAL_ADDRESS_MEMBERS = 103
OFFICIAL_ADDRESS_COMPRESSED_BYTES = 1_557_464_449
OFFICIAL_ADDRESS_UNCOMPRESSED_BYTES = 31_460_865_738

DEFAULT_MIN_FREE_BYTES = 5 * 2**30
DEFAULT_MAX_DATABASE_BYTES = 2 * 2**30
DEFAULT_MAX_OUTPUT_BYTES = 2 * 2**30
DEFAULT_MAX_CANDIDATES = 5_000_000
DEFAULT_MAX_LOCALITIES = 100_000
DEFAULT_MAX_LOCALITY_POSTCODE_RELATIONS = 1_000_000
DEFAULT_BLOOM_BYTES = 32 * 2**20
DEFAULT_MAX_ADDRESS_MEMBERS = 128
DEFAULT_MAX_TOTAL_COMPRESSED_BYTES = 2 * 2**30
DEFAULT_MAX_TOTAL_UNCOMPRESSED_BYTES = 40 * 2**30
DEFAULT_MAX_NETWORK_BYTES = 4 * 2**30
DEFAULT_MAX_MEMBER_COMPRESSED_BYTES = 32 * 2**20
DEFAULT_MAX_MEMBER_UNCOMPRESSED_BYTES = 512 * 2**20
DEFAULT_COMPRESSED_CHUNK_BYTES = 64 * 2**10
DEFAULT_XML_CHUNK_BYTES = 2 * 2**20
DEFAULT_MAX_MEMBER_ATTEMPTS = 2
DEFAULT_MEMBER_CHECK_RECORDS = 10_000
DEFAULT_OUTPUT_CHECK_ROWS = 10_000
DEFAULT_BASE_CHECK_ROWS = 100_000
MAX_TEXT_CHARACTERS = 512
MAX_XML_DECLARATION_BYTES = 1024

_SHA256 = re.compile(r"[0-9a-f]{64}")
_POSTCODE = re.compile(r"[0-9]{5}")
_MUNICIPALITY_KEY = re.compile(r"[0-9]{8}")
_SIMPLE_HOUSE = re.compile(r"\s*([0-9]+)\s*([A-Za-z]?)\s*")
_CENTRAL_SIGNATURE = b"PK\x01\x02"
_LOCAL_SIGNATURE = b"PK\x03\x04"
_EOCD_SIGNATURE = b"PK\x05\x06"
_ADDRESS_FAMILIES = frozenset(
    {
        "EinheitenBiomasse",
        "EinheitenGasErzeuger",
        "EinheitenGasSpeicher",
        "EinheitenGasverbraucher",
        "EinheitenGeothermieGrubengasDruckentspannung",
        "EinheitenKernkraft",
        "EinheitenSolar",
        "EinheitenStromSpeicher",
        "EinheitenStromVerbraucher",
        "EinheitenVerbrennung",
        "EinheitenWasser",
        "EinheitenWind",
    }
)
_RELEVANT_XML_FIELDS = frozenset(
    {
        "Land",
        "Gemeinde",
        "Gemeindeschluessel",
        "Postleitzahl",
        "Strasse",
        "StrasseNichtGefunden",
        "Hausnummer",
        "Hausnummer_nv",
        "HausnummerNichtGefunden",
        "Adresszusatz",
        "Ort",
        "Laengengrad",
        "Breitengrad",
        "NetzbetreiberpruefungStatus",
        "EinheitSystemstatus",
        "EinheitBetriebsstatus",
    }
)


class MastrSupplementError(RuntimeError):
    """The MaStR supplement cannot continue without weakening a guard."""


class RetryableMastrSupplementError(MastrSupplementError):
    """One exact range attempt failed for a transient transport reason."""


@dataclass(frozen=True)
class Pin:
    sha256: str
    bytes: int


@dataclass(frozen=True)
class HttpContract:
    url: str
    content_length: int
    etag: str
    last_modified: str
    content_type: str
    central_directory_offset: int
    central_directory_bytes: int
    central_directory_sha256: str
    eocd_offset: int
    entries: int
    range_status: int = 206
    accept_ranges: str = "bytes"


@dataclass(frozen=True)
class Member:
    name: str
    local_header_offset: int
    range_end_exclusive: int
    compressed_bytes: int
    uncompressed_bytes: int
    crc32: int
    method: int
    flags: int

    @property
    def range_bytes(self) -> int:
        return self.range_end_exclusive - self.local_header_offset


@dataclass(frozen=True)
class ZipManifest:
    source: HttpContract
    members: tuple[Member, ...]


@dataclass(frozen=True)
class SourceProjection:
    street_norm: str
    number: int
    suffix: str
    locality_norm: str
    municipality_norm: str
    municipality_key: str
    postcode: str
    lon: str
    lat: str
    street_display: str
    locality_display: str
    municipality_display: str

    @property
    def semantic_key(self) -> tuple[str, int, str, str, str, str]:
        return (
            self.street_norm,
            self.number,
            self.suffix,
            self.locality_norm,
            self.municipality_norm,
            self.municipality_key,
        )

    @property
    def projection_signature(self) -> tuple[str, str, str]:
        return self.postcode, self.lon, self.lat


@dataclass(frozen=True)
class ResolvedProjection:
    identity: tuple[str, str, int, str]
    row: dict[str, str]


@dataclass
class Locality:
    code: str
    norm: str
    display: str
    province: str


@dataclass(frozen=True)
class Config:
    manifest: Path
    manifest_pin: Pin
    builder_csv: Path
    builder_pin: Pin
    expected_builder_rows: int
    sqlite_path: Path
    output_csv: Path
    receipt: Path
    expected_source: HttpContract | None = None
    minimum_free_bytes: int = DEFAULT_MIN_FREE_BYTES
    max_database_bytes: int = DEFAULT_MAX_DATABASE_BYTES
    max_output_bytes: int = DEFAULT_MAX_OUTPUT_BYTES
    max_candidates: int = DEFAULT_MAX_CANDIDATES
    max_localities: int = DEFAULT_MAX_LOCALITIES
    max_locality_postcode_relations: int = DEFAULT_MAX_LOCALITY_POSTCODE_RELATIONS
    bloom_bytes: int = DEFAULT_BLOOM_BYTES
    max_address_members: int = DEFAULT_MAX_ADDRESS_MEMBERS
    max_total_compressed_bytes: int = DEFAULT_MAX_TOTAL_COMPRESSED_BYTES
    max_total_uncompressed_bytes: int = DEFAULT_MAX_TOTAL_UNCOMPRESSED_BYTES
    max_network_bytes: int = DEFAULT_MAX_NETWORK_BYTES
    max_member_compressed_bytes: int = DEFAULT_MAX_MEMBER_COMPRESSED_BYTES
    max_member_uncompressed_bytes: int = DEFAULT_MAX_MEMBER_UNCOMPRESSED_BYTES
    max_member_attempts: int = DEFAULT_MAX_MEMBER_ATTEMPTS
    base_check_rows: int = DEFAULT_BASE_CHECK_ROWS


class RangeFetcher(Protocol):
    def open_range(self, start: int, end_exclusive: int) -> ContextManager[BinaryIO]:
        """Open one exact byte range as a binary response."""


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


def official_http_contract() -> HttpContract:
    return HttpContract(
        url=OFFICIAL_SOURCE_URL,
        content_length=OFFICIAL_CONTENT_LENGTH,
        etag=OFFICIAL_ETAG,
        last_modified=OFFICIAL_LAST_MODIFIED,
        content_type=OFFICIAL_CONTENT_TYPE,
        central_directory_offset=OFFICIAL_CENTRAL_DIRECTORY_OFFSET,
        central_directory_bytes=OFFICIAL_CENTRAL_DIRECTORY_BYTES,
        central_directory_sha256=OFFICIAL_CENTRAL_DIRECTORY_SHA256,
        eocd_offset=OFFICIAL_EOCD_OFFSET,
        entries=OFFICIAL_ZIP_ENTRIES,
    )


def _validate_pin(pin: Pin, label: str) -> None:
    if not isinstance(pin.sha256, str) or _SHA256.fullmatch(pin.sha256) is None:
        raise MastrSupplementError(f"{label} SHA-256 pin is invalid")
    if isinstance(pin.bytes, bool) or not isinstance(pin.bytes, int) or pin.bytes <= 0:
        raise MastrSupplementError(f"{label} byte pin must be positive")


def _file_evidence(path: Path) -> dict[str, Any]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(2**20), b""):
            digest.update(chunk)
            size += len(chunk)
    return {"path": str(path), "bytes": size, "sha256": digest.hexdigest()}


def _read_pinned(path: Path, pin: Pin, label: str) -> bytes:
    _validate_pin(pin, label)
    try:
        payload = path.read_bytes()
    except OSError as exc:
        raise MastrSupplementError(f"{label} is unavailable: {path}") from exc
    if len(payload) != pin.bytes or hashlib.sha256(payload).hexdigest() != pin.sha256:
        raise MastrSupplementError(f"{label} pin mismatch")
    return payload


def _strict_json(raw: bytes, label: str) -> Any:
    def pairs(values: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in values:
            if key in result:
                raise MastrSupplementError(f"{label} has duplicate JSON key {key!r}")
            result[key] = value
        return result

    def constant(value: str) -> None:
        raise MastrSupplementError(f"{label} has non-finite JSON value {value}")

    try:
        return json.loads(
            raw.decode("utf-8", errors="strict"),
            object_pairs_hook=pairs,
            parse_constant=constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise MastrSupplementError(f"{label} is not strict UTF-8 JSON: {exc}") from exc


def _plain(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip() or "\x00" in value:
        raise MastrSupplementError(f"{label} must be a non-empty string")
    result = value.strip()
    if len(result) > MAX_TEXT_CHARACTERS:
        raise MastrSupplementError(f"{label} exceeds the bounded text length")
    return result


def _positive_int(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise MastrSupplementError(f"{label} must be a positive integer")
    return value


def _http_contract_from_json(value: Any) -> HttpContract:
    if not isinstance(value, dict) or set(value) != {
        "accept_ranges",
        "central_directory_bytes",
        "central_directory_offset",
        "central_directory_sha256",
        "content_length",
        "content_type",
        "eocd_offset",
        "entries",
        "etag",
        "last_modified",
        "range_status",
        "url",
    }:
        raise MastrSupplementError("manifest source contract shape drift")
    source = HttpContract(
        url=_plain(value["url"], "source URL"),
        content_length=_positive_int(value["content_length"], "content length"),
        etag=_plain(value["etag"], "source ETag"),
        last_modified=_plain(value["last_modified"], "source Last-Modified"),
        content_type=_plain(value["content_type"], "source Content-Type"),
        central_directory_offset=_positive_int(
            value["central_directory_offset"], "central directory offset"
        ),
        central_directory_bytes=_positive_int(
            value["central_directory_bytes"], "central directory bytes"
        ),
        central_directory_sha256=_plain(
            value["central_directory_sha256"], "central directory SHA-256"
        ),
        eocd_offset=_positive_int(value["eocd_offset"], "EOCD offset"),
        entries=_positive_int(value["entries"], "ZIP entries"),
        range_status=_positive_int(value["range_status"], "range status"),
        accept_ranges=_plain(value["accept_ranges"], "Accept-Ranges"),
    )
    if source.range_status != 206 or source.accept_ranges.lower() != "bytes":
        raise MastrSupplementError("source is not pinned to exact HTTP byte ranges")
    if not source.url.startswith("https://"):
        raise MastrSupplementError("source URL must use HTTPS")
    if _SHA256.fullmatch(source.central_directory_sha256) is None:
        raise MastrSupplementError("central directory SHA-256 pin is invalid")
    if source.central_directory_offset + source.central_directory_bytes != source.eocd_offset:
        raise MastrSupplementError("central directory does not end at the EOCD")
    if source.eocd_offset + 22 != source.content_length:
        raise MastrSupplementError("only a zero-comment, non-ZIP64 archive is accepted")
    return source


def _source_json(source: HttpContract) -> dict[str, Any]:
    return {
        "url": source.url,
        "content_length": source.content_length,
        "etag": source.etag,
        "last_modified": source.last_modified,
        "content_type": source.content_type,
        "range_status": source.range_status,
        "accept_ranges": source.accept_ranges,
        "central_directory_offset": source.central_directory_offset,
        "central_directory_bytes": source.central_directory_bytes,
        "central_directory_sha256": source.central_directory_sha256,
        "eocd_offset": source.eocd_offset,
        "entries": source.entries,
    }


def _member_from_json(value: Any) -> Member:
    if not isinstance(value, dict) or set(value) != {
        "compressed_bytes",
        "crc32",
        "flags",
        "local_header_offset",
        "method",
        "name",
        "range_end_exclusive",
        "uncompressed_bytes",
    }:
        raise MastrSupplementError("manifest member shape drift")
    member = Member(
        name=_plain(value["name"], "member name"),
        local_header_offset=value["local_header_offset"],
        range_end_exclusive=value["range_end_exclusive"],
        compressed_bytes=value["compressed_bytes"],
        uncompressed_bytes=value["uncompressed_bytes"],
        crc32=value["crc32"],
        method=value["method"],
        flags=value["flags"],
    )
    numeric = (
        member.local_header_offset,
        member.range_end_exclusive,
        member.compressed_bytes,
        member.uncompressed_bytes,
        member.crc32,
        member.method,
        member.flags,
    )
    if any(isinstance(item, bool) or not isinstance(item, int) for item in numeric):
        raise MastrSupplementError("manifest member numeric field drift")
    if (
        member.local_header_offset < 0
        or member.range_end_exclusive <= member.local_header_offset
        or member.compressed_bytes <= 0
        or member.uncompressed_bytes <= 0
        or not (0 <= member.crc32 <= 0xFFFF_FFFF)
        or member.method != 8
        or member.flags != 0
    ):
        raise MastrSupplementError(f"unsupported ZIP member contract: {member.name}")
    if "/" in member.name or "\\" in member.name or not member.name.endswith(".xml"):
        raise MastrSupplementError(f"unsafe ZIP member name: {member.name}")
    return member


def manifest_json(manifest: ZipManifest) -> dict[str, Any]:
    return {
        "schema": MANIFEST_SCHEMA,
        "source": _source_json(manifest.source),
        "members": [
            {
                "name": member.name,
                "local_header_offset": member.local_header_offset,
                "range_end_exclusive": member.range_end_exclusive,
                "compressed_bytes": member.compressed_bytes,
                "uncompressed_bytes": member.uncompressed_bytes,
                "crc32": member.crc32,
                "method": member.method,
                "flags": member.flags,
            }
            for member in manifest.members
        ],
    }


def parse_manifest(raw: bytes) -> ZipManifest:
    value = _strict_json(raw, "MaStR member manifest")
    if not isinstance(value, dict) or set(value) != {"members", "schema", "source"}:
        raise MastrSupplementError("manifest top-level shape drift")
    if value["schema"] != MANIFEST_SCHEMA:
        raise MastrSupplementError("manifest schema drift")
    source = _http_contract_from_json(value["source"])
    if not isinstance(value["members"], list):
        raise MastrSupplementError("manifest members must be a list")
    members = tuple(_member_from_json(item) for item in value["members"])
    if len(members) != source.entries:
        raise MastrSupplementError("manifest member count does not match the EOCD pin")
    if len({member.name for member in members}) != len(members):
        raise MastrSupplementError("manifest member names are not unique")
    ordered = sorted(members, key=lambda item: item.local_header_offset)
    if tuple(ordered) != members:
        raise MastrSupplementError("manifest members are not in local-header order")
    for index, member in enumerate(members):
        expected_end = (
            members[index + 1].local_header_offset
            if index + 1 < len(members)
            else source.central_directory_offset
        )
        if member.range_end_exclusive != expected_end:
            raise MastrSupplementError(f"member range is not contiguous: {member.name}")
        if member.range_end_exclusive > source.central_directory_offset:
            raise MastrSupplementError(f"member crosses central directory: {member.name}")
    return ZipManifest(source=source, members=members)


def manifest_from_tail(
    tail: bytes,
    *,
    tail_start: int,
    source: HttpContract,
) -> ZipManifest:
    """Parse and verify a complete central directory contained in a bounded tail."""

    if tail_start < 0 or tail_start + len(tail) != source.content_length:
        raise MastrSupplementError("ZIP tail byte range does not match Content-Length")
    relative_eocd = tail.rfind(_EOCD_SIGNATURE)
    if relative_eocd < 0 or tail_start + relative_eocd != source.eocd_offset:
        raise MastrSupplementError("pinned EOCD was not found at the expected offset")
    try:
        (
            signature,
            disk,
            central_disk,
            disk_entries,
            entries,
            central_bytes,
            central_offset,
            comment_bytes,
        ) = struct.unpack_from("<4s4H2LH", tail, relative_eocd)
    except struct.error as exc:
        raise MastrSupplementError("truncated EOCD") from exc
    if signature != _EOCD_SIGNATURE or disk or central_disk or comment_bytes:
        raise MastrSupplementError("multi-disk or commented ZIP is unsupported")
    if disk_entries != entries or entries != source.entries:
        raise MastrSupplementError("EOCD entry-count pin mismatch")
    if (
        central_bytes != source.central_directory_bytes
        or central_offset != source.central_directory_offset
    ):
        raise MastrSupplementError("EOCD central-directory pin mismatch")
    relative_central = central_offset - tail_start
    central = tail[relative_central : relative_central + central_bytes]
    if len(central) != central_bytes:
        raise MastrSupplementError("tail does not contain the complete central directory")
    if hashlib.sha256(central).hexdigest() != source.central_directory_sha256:
        raise MastrSupplementError("central-directory SHA-256 pin mismatch")

    parsed: list[Member] = []
    position = 0
    while position < len(central):
        try:
            header = struct.unpack_from("<4s6H3L5H2L", central, position)
        except struct.error as exc:
            raise MastrSupplementError("truncated central-directory header") from exc
        if header[0] != _CENTRAL_SIGNATURE:
            raise MastrSupplementError("central-directory signature drift")
        flags, method = header[3], header[4]
        crc32, compressed, uncompressed = header[7], header[8], header[9]
        name_bytes, extra_bytes, comment = header[10], header[11], header[12]
        local_offset = header[16]
        end = position + 46 + name_bytes + extra_bytes + comment
        if end > len(central):
            raise MastrSupplementError("central-directory variable fields are truncated")
        encoding = "utf-8" if flags & 0x0800 else "cp437"
        try:
            name = central[position + 46 : position + 46 + name_bytes].decode(
                encoding, errors="strict"
            )
        except UnicodeDecodeError as exc:
            raise MastrSupplementError("member name encoding drift") from exc
        parsed.append(
            Member(
                name=name,
                local_header_offset=local_offset,
                range_end_exclusive=0,
                compressed_bytes=compressed,
                uncompressed_bytes=uncompressed,
                crc32=crc32,
                method=method,
                flags=flags,
            )
        )
        position = end
    if len(parsed) != source.entries or position != len(central):
        raise MastrSupplementError("central-directory conservation failed")
    parsed.sort(key=lambda item: item.local_header_offset)
    completed = tuple(
        Member(
            name=member.name,
            local_header_offset=member.local_header_offset,
            range_end_exclusive=(
                parsed[index + 1].local_header_offset
                if index + 1 < len(parsed)
                else source.central_directory_offset
            ),
            compressed_bytes=member.compressed_bytes,
            uncompressed_bytes=member.uncompressed_bytes,
            crc32=member.crc32,
            method=member.method,
            flags=member.flags,
        )
        for index, member in enumerate(parsed)
    )
    # Reuse the strict JSON validator so both construction paths have one contract.
    return parse_manifest(canonical_json_bytes(manifest_json(ZipManifest(source, completed))))


def _member_family(name: str) -> str | None:
    match = re.fullmatch(r"(Einheiten[^/]+?)(?:_[0-9]+)?\.xml", name)
    if match is None:
        return None
    return match.group(1)


def selected_address_members(manifest: ZipManifest) -> tuple[Member, ...]:
    selected = tuple(
        member
        for member in manifest.members
        if _member_family(member.name) in _ADDRESS_FAMILIES
    )
    if not selected:
        raise MastrSupplementError("address-member selection is vacuous")
    return selected


def _selected_member_totals(members: Sequence[Member]) -> tuple[int, int, int]:
    return (
        len(members),
        sum(member.compressed_bytes for member in members),
        sum(member.uncompressed_bytes for member in members),
    )


def _validate_official_inventory(manifest: ZipManifest) -> None:
    if manifest.source != official_http_contract():
        raise MastrSupplementError("manifest source is not the exact official snapshot")
    totals = _selected_member_totals(selected_address_members(manifest))
    expected = (
        OFFICIAL_ADDRESS_MEMBERS,
        OFFICIAL_ADDRESS_COMPRESSED_BYTES,
        OFFICIAL_ADDRESS_UNCOMPRESSED_BYTES,
    )
    if totals != expected:
        raise MastrSupplementError(
            f"official address-member inventory drift: got {totals}, expected {expected}"
        )


def fetch_remote_manifest(
    source: HttpContract,
    fetcher: RangeFetcher,
) -> tuple[ZipManifest, dict[str, Any]]:
    """Fetch only the bounded central-directory tail and derive its inventory."""

    start = source.central_directory_offset
    expected_bytes = source.content_length - start
    with fetcher.open_range(start, source.content_length) as stream:
        tail = _read_exact(stream, expected_bytes, "central directory and EOCD")
        if _read_stream(stream, 1, "central-directory trailer"):
            raise MastrSupplementError("central-directory range has trailing bytes")
    observed = manifest_from_tail(tail, tail_start=start, source=source)
    evidence = {
        "range_start": start,
        "range_end_exclusive": source.content_length,
        "bytes": expected_bytes,
        "sha256": hashlib.sha256(tail).hexdigest(),
    }
    return observed, evidence


def verify_remote_manifest(
    manifest: ZipManifest,
    fetcher: RangeFetcher,
) -> dict[str, Any]:
    """Bind the pinned JSON inventory to the source's exact central directory."""

    observed, evidence = fetch_remote_manifest(manifest.source, fetcher)
    if observed != manifest:
        raise MastrSupplementError(
            "pinned member manifest disagrees with the remote central directory"
        )
    return evidence


def write_official_manifest(
    output: Path,
    fetcher: RangeFetcher | None = None,
    *,
    minimum_free_bytes: int = DEFAULT_MIN_FREE_BYTES,
) -> dict[str, Any]:
    """Create the canonical official inventory with one bounded exact range."""

    if output.exists():
        raise MastrSupplementError(f"manifest output must not exist: {output}")
    if not output.parent.is_dir():
        raise MastrSupplementError(f"manifest output parent is missing: {output.parent}")
    if minimum_free_bytes < 0:
        raise MastrSupplementError("minimum free bytes must be non-negative")
    free = shutil.disk_usage(output.parent).free
    if free < minimum_free_bytes:
        raise MastrSupplementError(
            f"disk floor crossed before manifest creation: {free} < {minimum_free_bytes}"
        )
    source = official_http_contract()
    if fetcher is None:
        fetcher = HttpRangeFetcher(source)
    manifest, tail_evidence = fetch_remote_manifest(source, fetcher)
    _validate_official_inventory(manifest)
    payload = canonical_json_bytes(manifest_json(manifest))
    with output.open("xb") as handle:
        handle.write(payload)
        handle.flush()
        os.fsync(handle.fileno())
    return {
        "manifest": _file_evidence(output),
        "central_tail": tail_evidence,
        "selected": {
            "members": OFFICIAL_ADDRESS_MEMBERS,
            "compressed_bytes": OFFICIAL_ADDRESS_COMPRESSED_BYTES,
            "uncompressed_bytes": OFFICIAL_ADDRESS_UNCOMPRESSED_BYTES,
        },
    }


class HttpRangeFetcher:
    """Sequential exact-range client pinned to immutable response headers."""

    def __init__(
        self,
        source: HttpContract,
        *,
        opener: Callable[..., BinaryIO] = urllib.request.urlopen,
        timeout_seconds: int = 120,
    ) -> None:
        self.source = source
        self.opener = opener
        self.timeout_seconds = timeout_seconds

    @contextmanager
    def open_range(self, start: int, end_exclusive: int) -> Iterator[BinaryIO]:
        if start < 0 or end_exclusive <= start or end_exclusive > self.source.content_length:
            raise MastrSupplementError("requested HTTP range is outside the source pin")
        request = urllib.request.Request(
            self.source.url,
            headers={
                "Accept-Encoding": "identity",
                "If-Match": self.source.etag,
                "Range": f"bytes={start}-{end_exclusive - 1}",
                "User-Agent": "GridPin-MaStR-source-builder/1.0",
            },
            method="GET",
        )
        try:
            response = self.opener(request, timeout=self.timeout_seconds)
        except (OSError, http.client.HTTPException) as exc:
            raise RetryableMastrSupplementError(
                f"MaStR range request failed: {exc}"
            ) from exc
        body_completed = False
        try:
            status = getattr(response, "status", None)
            headers = getattr(response, "headers", None)
            if status != self.source.range_status or headers is None:
                raise MastrSupplementError("MaStR response is not exact HTTP 206")
            expected_bytes = end_exclusive - start
            expected_range = (
                f"bytes {start}-{end_exclusive - 1}/{self.source.content_length}"
            )
            checks = {
                "Content-Range": expected_range,
                "Content-Length": str(expected_bytes),
                "ETag": self.source.etag,
                "Last-Modified": self.source.last_modified,
                "Accept-Ranges": self.source.accept_ranges,
            }
            for name, expected in checks.items():
                actual = headers.get(name)
                if actual != expected:
                    raise MastrSupplementError(
                        f"MaStR {name} drift: got {actual!r}, expected {expected!r}"
                    )
            content_type = headers.get("Content-Type", "").split(";", 1)[0].strip()
            if content_type != self.source.content_type:
                raise MastrSupplementError("MaStR Content-Type drift")
            if headers.get("Content-Encoding") not in (None, "", "identity"):
                raise MastrSupplementError("encoded HTTP ranges are forbidden")
            yield response
            body_completed = True
        finally:
            close = getattr(response, "close", None)
            if callable(close):
                try:
                    close()
                except (OSError, http.client.HTTPException) as exc:
                    if body_completed:
                        raise RetryableMastrSupplementError(
                            f"MaStR range close failed: {exc}"
                        ) from exc


def _read_stream(stream: BinaryIO, size: int, label: str) -> bytes:
    try:
        return stream.read(size)
    except (OSError, http.client.HTTPException) as exc:
        raise RetryableMastrSupplementError(
            f"MaStR range read failed for {label}: {exc}"
        ) from exc


def _read_exact(stream: BinaryIO, size: int, label: str) -> bytes:
    chunks: list[bytes] = []
    remaining = size
    while remaining:
        chunk = _read_stream(stream, remaining, label)
        if not chunk:
            raise RetryableMastrSupplementError(f"truncated {label}")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _record_element(name: str) -> tuple[str, str]:
    family = _member_family(name)
    if family not in _ADDRESS_FAMILIES:
        raise MastrSupplementError(f"member is not an address-bearing unit XML: {name}")
    return family, "Einheit" + family[len("Einheiten") :]


def _local_tag(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def _record_fields(element: ET.Element) -> dict[str, str]:
    result: dict[str, str] = {}
    for child in element:
        name = _local_tag(child.tag)
        if name not in _RELEVANT_XML_FIELDS:
            continue
        if name in result:
            raise MastrSupplementError(f"duplicate XML field {name}")
        if list(child):
            raise MastrSupplementError(f"nested XML field {name}")
        result[name] = (child.text or "").strip()
    return result


def _bounded_text(value: str | None) -> str | None:
    if value is None or not value.strip() or "\x00" in value:
        return None
    result = value.strip()
    if len(result) > MAX_TEXT_CHARACTERS:
        return None
    return result


def _coordinate(value: str | None, *, latitude: bool) -> str | None:
    text = _bounded_text(value)
    if text is None:
        return None
    try:
        number = Decimal(text)
    except InvalidOperation:
        return None
    if not number.is_finite():
        return None
    lower, upper = (Decimal("47"), Decimal("56")) if latitude else (
        Decimal("5"),
        Decimal("16"),
    )
    if number < lower or number > upper:
        return None
    if number == 0:
        number = Decimal(0)
    rendered = format(number.normalize(), "f")
    return rendered


def strict_projection(fields: Mapping[str, str]) -> tuple[SourceProjection | None, str]:
    gates = (
        ("Land", "84", "excluded_non_germany"),
        ("NetzbetreiberpruefungStatus", "2954", "excluded_not_checked"),
        ("EinheitSystemstatus", "472", "excluded_not_active"),
        ("EinheitBetriebsstatus", "35", "excluded_not_in_operation"),
        ("StrasseNichtGefunden", "0", "excluded_street_not_bkg"),
        ("Hausnummer_nv", "0", "excluded_house_unavailable"),
        ("HausnummerNichtGefunden", "0", "excluded_house_not_bkg"),
    )
    for field, expected, reason in gates:
        if fields.get(field) != expected:
            return None, reason
    if _bounded_text(fields.get("Adresszusatz")) is not None:
        return None, "excluded_address_qualifier"
    postcode = _bounded_text(fields.get("Postleitzahl"))
    if postcode is None or _POSTCODE.fullmatch(postcode) is None:
        return None, "excluded_incomplete_postcode"
    house = _bounded_text(fields.get("Hausnummer"))
    match = _SIMPLE_HOUSE.fullmatch(house or "")
    if match is None:
        return None, "excluded_non_simple_house"
    number = int(match.group(1))
    if number <= 0 or number > 0xFFFF_FFFF:
        return None, "excluded_non_simple_house"
    suffix = builder.normalize_rep(match.group(2))
    street = _bounded_text(fields.get("Strasse"))
    locality = _bounded_text(fields.get("Ort"))
    municipality = _bounded_text(fields.get("Gemeinde"))
    municipality_key = _bounded_text(fields.get("Gemeindeschluessel"))
    if street is None:
        return None, "excluded_incomplete_street"
    if locality is None or municipality is None:
        return None, "excluded_incomplete_locality"
    if municipality_key is None or _MUNICIPALITY_KEY.fullmatch(municipality_key) is None:
        return None, "excluded_incomplete_municipality_key"
    street_norm = builder.normalize_text(street)
    locality_norm = builder.normalize_text(locality)
    municipality_norm = builder.normalize_text(municipality)
    if not street_norm or not locality_norm or not municipality_norm:
        return None, "excluded_empty_normalized_address"
    lon = _coordinate(fields.get("Laengengrad"), latitude=False)
    lat = _coordinate(fields.get("Breitengrad"), latitude=True)
    if lon is None or lat is None:
        return None, "excluded_incomplete_coordinates"
    return (
        SourceProjection(
            street_norm=street_norm,
            number=number,
            suffix=suffix,
            locality_norm=locality_norm,
            municipality_norm=municipality_norm,
            municipality_key=municipality_key,
            postcode=postcode,
            lon=lon,
            lat=lat,
            street_display=street,
            locality_display=locality,
            municipality_display=municipality,
        ),
        "admitted_strict_record",
    )


class ProjectionStore:
    """Bounded deterministic SQLite aggregation for source and base relations."""

    def __init__(
        self,
        path: Path,
        *,
        max_bytes: int,
        max_candidates: int,
        minimum_free_bytes: int = 0,
    ) -> None:
        if path.exists():
            raise MastrSupplementError(f"SQLite output must not exist: {path}")
        if not path.parent.is_dir():
            raise MastrSupplementError(f"SQLite parent is missing: {path.parent}")
        if max_bytes < 2**20 or max_candidates <= 0 or minimum_free_bytes < 0:
            raise MastrSupplementError("SQLite bounds are invalid")
        self.path = path
        self.max_bytes = max_bytes
        self.max_candidates = max_candidates
        self.minimum_free_bytes = minimum_free_bytes
        self.candidates = 0
        self._member_active = False
        self._member_candidate_start = 0
        self.connection = sqlite3.connect(path)
        try:
            self.connection.execute("PRAGMA page_size=4096")
            # DELETE journaling is required for member SAVEPOINT rollback.  The
            # main file plus its transient rollback journal share one byte cap.
            self.connection.execute("PRAGMA journal_mode=DELETE")
            self.connection.execute("PRAGMA synchronous=FULL")
            # Keep both the page cache and any unexpected SQLite temp work
            # bounded on disk.  The ordered scans below follow WITHOUT ROWID
            # primary keys and therefore should not need a temp sort at all.
            self.connection.execute("PRAGMA cache_size=-65536")
            self.connection.execute("PRAGMA temp_store=FILE")
            page_limit = int(
                self.connection.execute(
                    f"PRAGMA max_page_count={max_bytes // 4096}"
                ).fetchone()[0]
            )
            if page_limit * 4096 > max_bytes:
                raise MastrSupplementError("SQLite page ceiling was not applied")
            self.connection.executescript(
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
                ) WITHOUT ROWID;
                CREATE TABLE base_hit (
                    street_norm TEXT NOT NULL,
                    number INTEGER NOT NULL,
                    suffix TEXT NOT NULL,
                    code TEXT NOT NULL,
                    postcode TEXT NOT NULL,
                    PRIMARY KEY(street_norm, number, suffix, code, postcode)
                ) WITHOUT ROWID;
                CREATE TABLE locality (
                    code TEXT NOT NULL PRIMARY KEY,
                    locality_norm TEXT NOT NULL,
                    locality_display TEXT NOT NULL,
                    province TEXT NOT NULL
                ) WITHOUT ROWID;
                CREATE TABLE locality_relation (
                    locality_norm TEXT NOT NULL,
                    postcode TEXT NOT NULL,
                    code TEXT NOT NULL,
                    PRIMARY KEY(locality_norm, postcode, code)
                ) WITHOUT ROWID;
                CREATE TABLE processed_member (
                    name TEXT NOT NULL PRIMARY KEY,
                    ordinal INTEGER NOT NULL UNIQUE,
                    evidence_json TEXT NOT NULL,
                    counts_json TEXT NOT NULL
                ) WITHOUT ROWID;
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
                ) WITHOUT ROWID;
                """
            )
        except (sqlite3.Error, OSError) as exc:
            self.connection.close()
            raise MastrSupplementError(f"cannot initialise bounded SQLite: {exc}") from exc

    def _execute(self, sql: str, values: Sequence[Any] = ()) -> sqlite3.Cursor:
        try:
            return self.connection.execute(sql, values)
        except sqlite3.Error as exc:
            raise MastrSupplementError(f"bounded SQLite operation failed: {exc}") from exc

    def _measure_bound(self) -> None:
        try:
            physical_main = self.path.stat().st_size
            journal = Path(str(self.path) + "-journal")
            journal_size = journal.stat().st_size if journal.exists() else 0
        except OSError as exc:
            raise MastrSupplementError("bounded SQLite disappeared") from exc
        page_count = int(self._execute("PRAGMA page_count").fetchone()[0])
        page_size = int(self._execute("PRAGMA page_size").fetchone()[0])
        logical_main = page_count * page_size
        size = max(physical_main, logical_main) + journal_size
        if size > self.max_bytes:
            raise MastrSupplementError("bounded SQLite byte ceiling crossed")
        free = shutil.disk_usage(self.path.parent).free
        if free < self.minimum_free_bytes:
            raise MastrSupplementError(
                "disk floor crossed during MaStR aggregation: "
                f"{free} < {self.minimum_free_bytes}"
            )

    def check_bound(self) -> None:
        if self.connection.in_transaction:
            # Observe dirty logical pages plus the rollback journal before a
            # base-scan commit can delete the journal.
            self._measure_bound()
            if self._member_active:
                return
            self.connection.commit()
        self._measure_bound()

    def begin_member(self) -> None:
        if self._member_active:
            raise MastrSupplementError("nested member transaction is forbidden")
        self.connection.commit()
        self._execute("SAVEPOINT mastr_member")
        self._member_active = True
        self._member_candidate_start = self.candidates

    def finish_member(
        self,
        name: str,
        ordinal: int,
        evidence: Mapping[str, Any],
        counts: Mapping[str, int],
    ) -> None:
        if not self._member_active:
            raise MastrSupplementError("member transaction is not active")
        self._execute(
            "INSERT INTO processed_member VALUES(?,?,?,?)",
            (
                name,
                ordinal,
                canonical_json_bytes(dict(evidence)).decode("utf-8").rstrip("\n"),
                canonical_json_bytes(dict(sorted(counts.items())))
                .decode("utf-8")
                .rstrip("\n"),
            ),
        )
        # Count the checkpoint row and the final dirty-page/journal peak while
        # the savepoint can still be rolled back.
        self.check_bound()
        self._execute("RELEASE SAVEPOINT mastr_member")
        self._member_active = False
        self.connection.commit()

    def rollback_member(self) -> None:
        if not self._member_active:
            return
        try:
            self._execute("ROLLBACK TO SAVEPOINT mastr_member")
            self._execute("RELEASE SAVEPOINT mastr_member")
        finally:
            self._member_active = False
            self.candidates = self._member_candidate_start

    def processed_members(self) -> int:
        return int(
            self._execute("SELECT count(*) FROM processed_member").fetchone()[0]
        )

    def add(self, projection: SourceProjection) -> str:
        key = projection.semantic_key
        row = self._execute(
            """
            SELECT postcode, lon, lat, street_display, locality_display,
                   municipality_display, observations, conflict
            FROM candidate
            WHERE street_norm=? AND number=? AND suffix=? AND locality_norm=?
              AND municipality_norm=? AND municipality_key=?
            """,
            key,
        ).fetchone()
        if row is None:
            if self.candidates >= self.max_candidates:
                raise MastrSupplementError("source candidate ceiling crossed")
            self._execute(
                """
                INSERT INTO candidate VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,0)
                """,
                (
                    *key,
                    projection.postcode,
                    projection.lon,
                    projection.lat,
                    projection.street_display,
                    projection.locality_display,
                    projection.municipality_display,
                    1,
                ),
            )
            self.candidates += 1
            if self.candidates % 10_000 == 0:
                self.check_bound()
            return "unique_candidate"
        postcode, lon, lat, street, locality, municipality, observations, conflict = row
        if (postcode, lon, lat) != projection.projection_signature:
            self._execute(
                """
                UPDATE candidate SET observations=?, conflict=1
                WHERE street_norm=? AND number=? AND suffix=? AND locality_norm=?
                  AND municipality_norm=? AND municipality_key=?
                """,
                (observations + 1, *key),
            )
            return "conflicting_projection"
        self._execute(
            """
            UPDATE candidate
            SET observations=?, street_display=?, locality_display=?,
                municipality_display=?, conflict=?
            WHERE street_norm=? AND number=? AND suffix=? AND locality_norm=?
              AND municipality_norm=? AND municipality_key=?
            """,
            (
                observations + 1,
                min(street, projection.street_display),
                min(locality, projection.locality_display),
                min(municipality, projection.municipality_display),
                conflict,
                *key,
            ),
        )
        return "duplicate_projection"

    def iter_surfaces(self) -> Iterator[tuple[str, int, str]]:
        previous: tuple[str, int, str] | None = None
        rows = self._execute(
            "SELECT street_norm, number, suffix FROM candidate ORDER BY 1,2,3"
        )
        for street, number, suffix in rows:
            surface = (street, number, suffix)
            if surface != previous:
                yield surface
                previous = surface

    def has_surface(self, surface: tuple[str, int, str]) -> bool:
        return (
            self._execute(
                "SELECT 1 FROM candidate WHERE street_norm=? AND number=? AND suffix=? LIMIT 1",
                surface,
            ).fetchone()
            is not None
        )

    def add_base_hit(
        self,
        surface: tuple[str, int, str],
        code: str,
        postcode: str,
    ) -> None:
        self._execute(
            "INSERT OR IGNORE INTO base_hit VALUES(?,?,?,?,?)",
            (*surface, code, postcode),
        )

    def has_base_hit(self, surface: tuple[str, int, str], code: str) -> bool:
        return (
            self._execute(
                "SELECT 1 FROM base_hit WHERE street_norm=? AND number=? AND suffix=? AND code=?",
                (*surface, code),
            ).fetchone()
            is not None
        )

    def add_locality(self, locality: Locality) -> None:
        old = self._execute(
            "SELECT locality_norm, locality_display, province FROM locality WHERE code=?",
            (locality.code,),
        ).fetchone()
        if old is None:
            self._execute(
                "INSERT INTO locality VALUES(?,?,?,?)",
                (
                    locality.code,
                    locality.norm,
                    locality.display,
                    locality.province,
                ),
            )
            return
        norm, display, province = old
        if norm != locality.norm or (
            province and locality.province and province != locality.province
        ):
            raise MastrSupplementError(
                f"persisted locality projection conflicts for {locality.code}"
            )
        self._execute(
            "UPDATE locality SET locality_display=?, province=? WHERE code=?",
            (
                min(display, locality.display),
                province or locality.province,
                locality.code,
            ),
        )

    def add_locality_relation(self, norm: str, postcode: str, code: str) -> None:
        self._execute(
            "INSERT OR IGNORE INTO locality_relation VALUES(?,?,?)",
            (norm, postcode, code),
        )

    def iter_candidates(self) -> Iterator[tuple[Any, ...]]:
        yield from self._execute(
            """
            SELECT street_norm, number, suffix, locality_norm, municipality_norm,
                   municipality_key, postcode, lon, lat, street_display,
                   locality_display, municipality_display, observations, conflict
            FROM candidate ORDER BY 1,2,3,4,5,6
            """
        )

    def add_resolved(
        self,
        row: Mapping[str, str],
        observations: int,
        municipality_key: str,
    ) -> str:
        identity = (
            row["nom_voie_norm"],
            row["code_insee"],
            int(row["numero"]),
            row["rep"],
        )
        old = self._execute(
            """
            SELECT postcode, lon, lat, street_display, locality_norm,
                   locality_display, province, municipality_key,
                   observations, conflict
            FROM resolved WHERE street_norm=? AND code=? AND number=? AND suffix=?
            """,
            identity,
        ).fetchone()
        if old is None:
            self._execute(
                "INSERT INTO resolved VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,0)",
                (
                    *identity,
                    row["code_postal_display"],
                    row["lon"],
                    row["lat"],
                    row["nom_voie"],
                    row["nom_commune_norm"],
                    row["nom_commune"],
                    row["provincia_norm"],
                    municipality_key,
                    observations,
                ),
            )
            return "unique_resolved"
        (
            postcode,
            lon,
            lat,
            street,
            loc_norm,
            loc_display,
            province,
            old_municipality_key,
            count,
            conflict,
        ) = old
        signature = (postcode, lon, lat, loc_norm, province, old_municipality_key)
        new_signature = (
            row["code_postal_display"],
            row["lon"],
            row["lat"],
            row["nom_commune_norm"],
            row["provincia_norm"],
            municipality_key,
        )
        if signature != new_signature:
            self._execute(
                """
                UPDATE resolved SET observations=?, conflict=1
                WHERE street_norm=? AND code=? AND number=? AND suffix=?
                """,
                (count + observations, *identity),
            )
            return "conflicting_resolved_projection"
        self._execute(
            """
            UPDATE resolved SET observations=?, street_display=?, locality_display=?,
                                conflict=?
            WHERE street_norm=? AND code=? AND number=? AND suffix=?
            """,
            (
                count + observations,
                min(street, row["nom_voie"]),
                min(loc_display, row["nom_commune"]),
                conflict,
                *identity,
            ),
        )
        return "duplicate_resolved_projection"

    def iter_resolved(self) -> Iterator[ResolvedProjection]:
        rows = self._execute(
            """
            SELECT street_norm, code, number, suffix, postcode, lon, lat,
                   street_display, locality_norm, locality_display, province
            FROM resolved WHERE conflict=0 ORDER BY 1,2,3,4
            """
        )
        for (
            street,
            code,
            number,
            suffix,
            postcode,
            lon,
            lat,
            display,
            loc_norm,
            loc_display,
            province,
        ) in rows:
            row = {
                "nom_voie_norm": street,
                "code_insee": code,
                "nom_commune_norm": loc_norm,
                "code_postal": postcode,
                "code_postal_display": postcode,
                "numero": str(number),
                "rep": suffix,
                "lon": lon,
                "lat": lat,
                "nom_voie": display,
                "nom_commune": loc_display,
                "provincia_norm": province,
            }
            yield ResolvedProjection((street, code, number, suffix), row)

    def resolved_count(self) -> int:
        return int(
            self._execute("SELECT count(*) FROM resolved WHERE conflict=0").fetchone()[0]
        )

    def close(self) -> None:
        self.rollback_member()
        self.connection.rollback()
        self.connection.close()


class BloomFilter:
    def __init__(self, byte_count: int, hashes: int = 5) -> None:
        if byte_count < 1024 or hashes <= 0 or hashes > 8:
            raise MastrSupplementError("Bloom-filter bounds are invalid")
        self.bits = bytearray(byte_count)
        self.bit_count = byte_count * 8
        self.hashes = hashes

    @staticmethod
    def _payload(surface: tuple[str, int, str]) -> bytes:
        return f"{surface[0]}\x1f{surface[1]}\x1f{surface[2]}".encode("utf-8")

    def _positions(self, surface: tuple[str, int, str]) -> Iterator[int]:
        digest = hashlib.blake2b(self._payload(surface), digest_size=64).digest()
        for index in range(self.hashes):
            value = int.from_bytes(digest[index * 8 : index * 8 + 8], "little")
            yield value % self.bit_count

    def add(self, surface: tuple[str, int, str]) -> None:
        for position in self._positions(surface):
            self.bits[position >> 3] |= 1 << (position & 7)

    def __contains__(self, surface: tuple[str, int, str]) -> bool:
        return all(
            self.bits[position >> 3] & (1 << (position & 7))
            for position in self._positions(surface)
        )


class GuardedCsvWriter:
    """Small writer proxy that enforces output and free-space bounds in flight."""

    def __init__(
        self,
        writer: Any,
        path: Path,
        *,
        max_bytes: int,
        minimum_free_bytes: int,
        check_every_rows: int = DEFAULT_OUTPUT_CHECK_ROWS,
    ) -> None:
        if max_bytes <= 0 or minimum_free_bytes < 0 or check_every_rows <= 0:
            raise MastrSupplementError("output guard bounds are invalid")
        self.writer = writer
        self.path = path
        self.max_bytes = max_bytes
        self.minimum_free_bytes = minimum_free_bytes
        self.check_every_rows = check_every_rows
        self.rows = 0

    def check(self) -> None:
        try:
            size = self.path.stat().st_size
        except OSError as exc:
            raise MastrSupplementError("guarded output disappeared") from exc
        if size > self.max_bytes:
            raise MastrSupplementError(
                f"output byte ceiling crossed: {size} > {self.max_bytes}"
            )
        free = shutil.disk_usage(self.path.parent).free
        if free < self.minimum_free_bytes:
            raise MastrSupplementError(
                "disk floor crossed while writing MaStR output: "
                f"{free} < {self.minimum_free_bytes}"
            )

    def writerow(self, row: Mapping[str, str]) -> Any:
        result = self.writer.writerow(row)
        self.rows += 1
        if self.rows % self.check_every_rows == 0:
            self.check()
        return result


def _stream_member(
    member: Member,
    fetcher: RangeFetcher,
    store: ProjectionStore,
    counts: Counter[str],
    *,
    max_compressed_bytes: int,
    max_uncompressed_bytes: int,
) -> dict[str, Any]:
    if member.compressed_bytes > max_compressed_bytes:
        raise MastrSupplementError(f"compressed member ceiling crossed: {member.name}")
    if member.uncompressed_bytes > max_uncompressed_bytes:
        raise MastrSupplementError(f"expanded member ceiling crossed: {member.name}")
    root_name, record_name = _record_element(member.name)
    compressed_hash = hashlib.sha256()
    expanded_hash = hashlib.sha256()
    crc = 0
    expanded_bytes = 0
    member_records = 0
    member_admitted = 0
    parser = ET.XMLPullParser(events=("start", "end"))
    xml_prefix = bytearray()
    xml_started = False
    root_seen = False
    root_closed = False
    root_element: ET.Element | None = None
    depth = 0

    def process_xml(payload: bytes) -> None:
        nonlocal depth, member_records, member_admitted
        nonlocal root_seen, root_closed, root_element
        parser.feed(payload)
        for event, element in parser.read_events():
            tag = _local_tag(element.tag)
            if event == "start":
                if depth == 0:
                    if root_seen or tag != root_name:
                        raise MastrSupplementError(
                            f"XML root drift in {member.name}: {tag!r}"
                        )
                    root_seen = True
                    root_element = element
                elif depth == 1 and tag != record_name:
                    raise MastrSupplementError(
                        f"unexpected direct-root XML child in {member.name}: {tag!r}"
                    )
                depth += 1
                continue

            depth -= 1
            if depth < 0:
                raise MastrSupplementError(f"XML depth underflow in {member.name}")
            if depth == 0:
                if tag != root_name:
                    raise MastrSupplementError(
                        f"XML root end drift in {member.name}: {tag!r}"
                    )
                root_closed = True
                continue
            if tag != record_name:
                continue
            if depth != 1:
                raise MastrSupplementError(
                    f"XML record is not a direct root child in {member.name}"
                )
            member_records += 1
            counts["source_records_seen"] += 1
            projection, reason = strict_projection(_record_fields(element))
            counts[reason] += 1
            if projection is not None:
                member_admitted += 1
                counts[store.add(projection)] += 1
            if member_records % DEFAULT_MEMBER_CHECK_RECORDS == 0:
                store.check_bound()
            element.clear()
            if root_element is None:
                raise MastrSupplementError(
                    f"XML record precedes the root in {member.name}"
                )
            try:
                root_element.remove(element)
            except ValueError as exc:
                raise MastrSupplementError(
                    f"XML record is not a direct root child in {member.name}"
                ) from exc

    def feed(payload: bytes) -> None:
        nonlocal crc, expanded_bytes, xml_started
        if not payload:
            return
        expanded_bytes += len(payload)
        if expanded_bytes > member.uncompressed_bytes:
            raise RetryableMastrSupplementError(
                f"expanded member exceeds manifest: {member.name}"
            )
        crc = zlib.crc32(payload, crc)
        expanded_hash.update(payload)
        try:
            if not xml_started:
                xml_prefix.extend(payload)
                if len(xml_prefix) < 4:
                    return
                if xml_prefix.startswith(b"\xff\xfe"):
                    terminator = b"?\x00>\x00"
                elif xml_prefix.startswith(b"\xfe\xff"):
                    terminator = b"\x00?\x00>"
                else:
                    raise MastrSupplementError(
                        f"XML is not BOM-pinned UTF-16 in {member.name}"
                    )
                declaration_end = xml_prefix.find(terminator)
                if declaration_end < 0:
                    if len(xml_prefix) > MAX_XML_DECLARATION_BYTES:
                        raise MastrSupplementError(
                            f"XML declaration exceeds its bound in {member.name}"
                        )
                    return
                declaration_end += len(terminator)
                if declaration_end > MAX_XML_DECLARATION_BYTES:
                    raise MastrSupplementError(
                        f"XML declaration exceeds its bound in {member.name}"
                    )
                declaration = bytes(xml_prefix[:declaration_end]).decode(
                    "utf-16", errors="strict"
                )
                pattern = (
                    r"<\?xml\b(?=[^?]*\bencoding\s*=\s*(['\"])utf-16\1)"
                    r"[^?]*\?>"
                )
                if re.fullmatch(pattern, declaration, flags=re.IGNORECASE) is None:
                    raise MastrSupplementError(
                        f"XML declaration is not pinned to UTF-16 in {member.name}"
                    )
                buffered = bytes(xml_prefix)
                xml_prefix.clear()
                xml_started = True
                process_xml(buffered)
            else:
                process_xml(payload)
        except UnicodeDecodeError as exc:
            raise RetryableMastrSupplementError(
                f"invalid UTF-16 declaration in {member.name}: {exc}"
            ) from exc
        except ET.ParseError as exc:
            raise RetryableMastrSupplementError(
                f"invalid XML in {member.name}: {exc}"
            ) from exc

    with fetcher.open_range(member.local_header_offset, member.range_end_exclusive) as stream:
        fixed = _read_exact(stream, 30, f"local header for {member.name}")
        try:
            header = struct.unpack("<4s5H3L2H", fixed)
        except struct.error as exc:
            raise MastrSupplementError(f"invalid local header for {member.name}") from exc
        (
            signature,
            _version,
            flags,
            method,
            _time,
            _date,
            checksum,
            compressed,
            expanded,
            name_len,
            extra_len,
        ) = header
        if signature != _LOCAL_SIGNATURE:
            raise MastrSupplementError(f"local-header signature drift: {member.name}")
        if (
            flags != member.flags
            or method != member.method
            or checksum != member.crc32
            or compressed != member.compressed_bytes
            or expanded != member.uncompressed_bytes
        ):
            raise MastrSupplementError(f"local/central member disagreement: {member.name}")
        name_bytes = _read_exact(stream, name_len, f"member name for {member.name}")
        encoding = "utf-8" if flags & 0x0800 else "cp437"
        try:
            local_name = name_bytes.decode(encoding, errors="strict")
        except UnicodeDecodeError as exc:
            raise MastrSupplementError(
                f"local member name encoding drift: {member.name}"
            ) from exc
        if local_name != member.name:
            raise MastrSupplementError(f"local member name drift: {member.name}")
        _read_exact(stream, extra_len, f"member extra field for {member.name}")
        expected_range = 30 + name_len + extra_len + member.compressed_bytes
        if expected_range != member.range_bytes:
            raise MastrSupplementError(f"member range includes unpinned bytes: {member.name}")

        decoder = zlib.decompressobj(-zlib.MAX_WBITS)
        remaining = member.compressed_bytes
        while remaining:
            block = _read_stream(
                stream,
                min(DEFAULT_COMPRESSED_CHUNK_BYTES, remaining),
                f"compressed member {member.name}",
            )
            if not block:
                raise RetryableMastrSupplementError(
                    f"truncated compressed member: {member.name}"
                )
            remaining -= len(block)
            compressed_hash.update(block)
            pending = block
            while pending:
                try:
                    output = decoder.decompress(pending, DEFAULT_XML_CHUNK_BYTES)
                except zlib.error as exc:
                    raise RetryableMastrSupplementError(
                        f"invalid deflate stream in {member.name}: {exc}"
                    ) from exc
                pending = decoder.unconsumed_tail
                feed(output)
        try:
            feed(decoder.flush())
        except zlib.error as exc:
            raise RetryableMastrSupplementError(
                f"invalid deflate stream in {member.name}: {exc}"
            ) from exc
        if not decoder.eof or decoder.unused_data:
            raise RetryableMastrSupplementError(
                f"deflate boundary drift: {member.name}"
            )
        if _read_stream(stream, 1, f"member trailer {member.name}"):
            raise MastrSupplementError(f"range has trailing bytes: {member.name}")
    try:
        parser.close()
    except ET.ParseError as exc:
        raise RetryableMastrSupplementError(
            f"truncated XML in {member.name}: {exc}"
        ) from exc
    if expanded_bytes != member.uncompressed_bytes:
        raise RetryableMastrSupplementError(
            f"expanded-byte conservation failed: {member.name}"
        )
    if (
        not xml_started
        or not root_seen
        or not root_closed
        or depth != 0
    ):
        raise MastrSupplementError(f"XML structure conservation failed: {member.name}")
    if crc & 0xFFFF_FFFF != member.crc32:
        raise RetryableMastrSupplementError(
            f"member CRC-32 mismatch: {member.name}"
        )
    if member_records <= 0:
        raise MastrSupplementError(f"unit member is vacuous: {member.name}")
    return {
        "name": member.name,
        "compressed_bytes": member.compressed_bytes,
        "uncompressed_bytes": expanded_bytes,
        "crc32": f"{member.crc32:08x}",
        "compressed_sha256": compressed_hash.hexdigest(),
        "uncompressed_sha256": expanded_hash.hexdigest(),
        "records": member_records,
        "strict_records": member_admitted,
    }


def acquire(
    manifest: ZipManifest,
    fetcher: RangeFetcher,
    store: ProjectionStore,
    *,
    max_address_members: int,
    max_total_compressed_bytes: int,
    max_total_uncompressed_bytes: int,
    max_network_bytes: int,
    initial_network_bytes: int,
    max_member_compressed_bytes: int,
    max_member_uncompressed_bytes: int,
    max_member_attempts: int,
) -> tuple[list[dict[str, Any]], Counter[str]]:
    counts: Counter[str] = Counter()
    evidence: list[dict[str, Any]] = []
    members = selected_address_members(manifest)
    totals = _selected_member_totals(members)
    ceilings = (
        max_address_members,
        max_total_compressed_bytes,
        max_total_uncompressed_bytes,
    )
    if any(observed > ceiling for observed, ceiling in zip(totals, ceilings)):
        raise MastrSupplementError(
            f"aggregate address-member ceiling crossed: {totals} > {ceilings}"
        )
    counts["address_members_planned"] = totals[0]
    counts["compressed_bytes_planned"] = totals[1]
    counts["uncompressed_bytes_planned"] = totals[2]
    counts["network_range_bytes_requested"] = initial_network_bytes
    counts["network_range_bytes_planned"] = initial_network_bytes + sum(
        member.range_bytes for member in members
    )
    if counts["network_range_bytes_planned"] > max_network_bytes:
        raise MastrSupplementError(
            "planned network byte ceiling crossed: "
            f"{counts['network_range_bytes_planned']} > {max_network_bytes}"
        )
    for ordinal, member in enumerate(members):
        member_evidence: dict[str, Any] | None = None
        for attempt in range(1, max_member_attempts + 1):
            # A resource failure is terminal and must be detected before the
            # next exact range is charged or opened.
            store.check_bound()
            requested = counts["network_range_bytes_requested"] + member.range_bytes
            if requested > max_network_bytes:
                raise MastrSupplementError(
                    f"network byte ceiling crossed before {member.name}: "
                    f"{requested} > {max_network_bytes}"
                )
            counts["network_range_bytes_requested"] = requested
            member_counts: Counter[str] = Counter()
            store.begin_member()
            try:
                member_evidence = _stream_member(
                    member,
                    fetcher,
                    store,
                    member_counts,
                    max_compressed_bytes=max_member_compressed_bytes,
                    max_uncompressed_bytes=max_member_uncompressed_bytes,
                )
                member_evidence["attempts"] = attempt
                store.finish_member(
                    member.name,
                    ordinal,
                    member_evidence,
                    member_counts,
                )
            except RetryableMastrSupplementError as exc:
                store.rollback_member()
                counts["member_attempt_failures"] += 1
                if attempt >= max_member_attempts:
                    raise MastrSupplementError(
                        f"member failed after {attempt} exact attempts: "
                        f"{member.name}: {exc}"
                    ) from exc
                continue
            except Exception:
                store.rollback_member()
                raise
            counts.update(member_counts)
            counts["member_retries"] += attempt - 1
            break
        if member_evidence is None:
            raise MastrSupplementError(f"member attempt accounting failed: {member.name}")
        evidence.append(member_evidence)
        counts["address_members_processed"] += 1
        counts["compressed_bytes_streamed"] += member.compressed_bytes
        counts["uncompressed_bytes_streamed"] += member.uncompressed_bytes
        store.check_bound()
    observed = (
        counts["address_members_processed"],
        counts["compressed_bytes_streamed"],
        counts["uncompressed_bytes_streamed"],
    )
    if observed != totals:
        raise MastrSupplementError(
            f"address-member stream conservation failed: {observed} != {totals}"
        )
    if store.processed_members() != totals[0]:
        raise MastrSupplementError("processed-member checkpoint conservation failed")
    if counts["admitted_strict_record"] <= 0 or store.candidates <= 0:
        raise MastrSupplementError("strict MaStR acquisition is vacuous")
    counts["unique_semantic_candidates"] = store.candidates
    return evidence, counts


def _load_base_context(
    pinned: builder.PinnedFile,
    store: ProjectionStore,
    *,
    expected_rows: int,
    max_localities: int,
    max_locality_postcode_relations: int,
    bloom_bytes: int,
    check_every_rows: int,
) -> tuple[
    dict[str, Locality],
    dict[tuple[str, str], set[str]],
    Counter[str],
]:
    bloom = BloomFilter(bloom_bytes)
    surface_count = 0
    for surface in store.iter_surfaces():
        bloom.add(surface)
        surface_count += 1
    if surface_count != store.candidates:
        # Several locality projections may share a surface; only an upper bound is expected.
        if surface_count <= 0 or surface_count > store.candidates:
            raise MastrSupplementError("source surface accounting drift")

    counts: Counter[str] = Counter()
    localities: dict[str, Locality] = {}
    relations: dict[tuple[str, str], set[str]] = {}
    relation_count = 0
    previous: tuple[Any, ...] | None = None
    text, reader = bnetza._csv_reader(pinned)
    try:
        for row in reader:
            counts["base_rows_scanned"] += 1
            sort_key = bnetza._row_sort_key(row)
            if previous is not None and sort_key < previous:
                raise MastrSupplementError("builder is not canonically sorted")
            previous = sort_key
            street, code, number, suffix = bnetza._row_identity(row)
            norm = row["nom_commune_norm"].strip()
            display = row["nom_commune"].strip()
            province = row["provincia_norm"].strip()
            if not code or not norm or not display:
                raise MastrSupplementError("builder locality is incomplete")
            locality = localities.get(code)
            if locality is None:
                if len(localities) >= max_localities:
                    raise MastrSupplementError("builder locality ceiling crossed")
                locality = Locality(code, norm, display, province)
                localities[code] = locality
                store.add_locality(locality)
            elif locality.norm != norm:
                raise MastrSupplementError(f"builder locality {code} maps to two names")
            elif locality.province and province and locality.province != province:
                raise MastrSupplementError(
                    f"builder locality {code} maps to two provinces"
                )
            elif not locality.province and province:
                locality.province = province
                store.add_locality(locality)
            postcode = builder._postcode(row)
            if postcode:
                relation = relations.setdefault((norm, postcode), set())
                if code not in relation:
                    if relation_count >= max_locality_postcode_relations:
                        raise MastrSupplementError(
                            "builder locality/postcode relation ceiling crossed"
                        )
                    relation.add(code)
                    relation_count += 1
                    store.add_locality_relation(norm, postcode, code)
            surface = (street, number, suffix)
            if surface in bloom and store.has_surface(surface):
                store.add_base_hit(surface, code, postcode)
                counts["base_rows_matching_source_surface"] += 1
            if counts["base_rows_scanned"] % check_every_rows == 0:
                store.check_bound()
                counts["base_bound_checks"] += 1
    finally:
        text.close()
    store.check_bound()
    counts["base_bound_checks"] += 1
    if counts["base_rows_scanned"] != expected_rows:
        raise MastrSupplementError(
            "builder row-count pin mismatch: "
            f"got {counts['base_rows_scanned']}, expected {expected_rows}"
        )
    counts["builder_localities"] = len(localities)
    counts["builder_locality_postcode_relations"] = relation_count
    counts["source_surfaces"] = surface_count
    return localities, relations, counts


def _resolve(
    store: ProjectionStore,
    localities: Mapping[str, Locality],
    relations: Mapping[tuple[str, str], set[str]],
) -> Counter[str]:
    counts: Counter[str] = Counter()
    for value in store.iter_candidates():
        (
            street,
            number,
            suffix,
            locality_norm,
            municipality_norm,
            municipality_key,
            postcode,
            lon,
            lat,
            street_display,
            _locality_display,
            _municipality_display,
            observations,
            conflict,
        ) = value
        if conflict:
            counts["excluded_conflicting_source_projection"] += 1
            continue
        aliases = {locality_norm, municipality_norm}
        codes: set[str] = set()
        for alias in aliases:
            codes.update(relations.get((alias, postcode), set()))
        if not codes:
            counts["excluded_no_product_locality_relation"] += 1
            continue
        if len(codes) != 1:
            counts["excluded_ambiguous_product_locality_relation"] += 1
            continue
        code = next(iter(codes))
        surface = (street, number, suffix)
        if store.has_base_hit(surface, code):
            counts["excluded_existing_base_identity"] += 1
            continue
        locality = localities[code]
        row = {
            "nom_voie_norm": street,
            "code_insee": code,
            "nom_commune_norm": locality.norm,
            "code_postal": postcode,
            "code_postal_display": postcode,
            "numero": str(number),
            "rep": suffix,
            "lon": lon,
            "lat": lat,
            "nom_voie": street_display,
            "nom_commune": locality.display,
            "provincia_norm": locality.province,
        }
        counts[store.add_resolved(row, observations, municipality_key)] += 1
    store.check_bound()
    counts["source_only_rows_resolved"] = store.resolved_count()
    if counts["source_only_rows_resolved"] <= 0:
        raise MastrSupplementError("strict source-only resolution is vacuous")
    return counts


def _validate_config(config: Config) -> None:
    for label, pin in (
        ("manifest", config.manifest_pin),
        ("builder", config.builder_pin),
    ):
        _validate_pin(pin, label)
    if config.expected_builder_rows <= 0:
        raise MastrSupplementError("expected builder rows must be positive")
    if config.builder_csv.suffix.lower() != ".gz":
        raise MastrSupplementError(
            "accepted builder must be gzip so CSV wrappers preserve its pinned fd"
        )
    for path in (config.sqlite_path, config.output_csv, config.receipt):
        if path.exists():
            raise MastrSupplementError(f"output must not exist: {path}")
        if not path.parent.is_dir():
            raise MastrSupplementError(f"output parent is missing: {path.parent}")
    if config.minimum_free_bytes < 0:
        raise MastrSupplementError("minimum free bytes must be non-negative")
    for value, label in (
        (config.max_database_bytes, "database bytes"),
        (config.max_output_bytes, "output bytes"),
        (config.max_candidates, "candidate count"),
        (config.max_localities, "locality count"),
        (
            config.max_locality_postcode_relations,
            "locality/postcode relation count",
        ),
        (config.bloom_bytes, "Bloom bytes"),
        (config.max_address_members, "address member count"),
        (config.max_total_compressed_bytes, "total compressed bytes"),
        (config.max_total_uncompressed_bytes, "total expanded bytes"),
        (config.max_network_bytes, "network bytes"),
        (config.max_member_compressed_bytes, "compressed member bytes"),
        (config.max_member_uncompressed_bytes, "expanded member bytes"),
    ):
        if value <= 0:
            raise MastrSupplementError(f"maximum {label} must be positive")
    if config.max_member_attempts < 1 or config.max_member_attempts > 3:
        raise MastrSupplementError("member attempts must be between one and three")
    if config.base_check_rows <= 0:
        raise MastrSupplementError("base check interval must be positive")


def build(config: Config, fetcher: RangeFetcher | None = None) -> dict[str, Any]:
    """Acquire the pinned source, resolve additions, and write a new builder."""

    _validate_config(config)
    free = min(
        shutil.disk_usage(path.parent).free
        for path in (config.sqlite_path, config.output_csv, config.receipt)
    )
    if free < config.minimum_free_bytes:
        raise MastrSupplementError(
            f"disk floor crossed before MaStR build: {free} < {config.minimum_free_bytes}"
        )
    manifest_raw = _read_pinned(config.manifest, config.manifest_pin, "member manifest")
    manifest_input_evidence = {
        "path": str(config.manifest),
        "bytes": len(manifest_raw),
        "sha256": hashlib.sha256(manifest_raw).hexdigest(),
    }
    manifest = parse_manifest(manifest_raw)
    expected_source = config.expected_source or official_http_contract()
    if manifest.source != expected_source:
        raise MastrSupplementError("manifest does not match the expected source contract")
    if expected_source == official_http_contract():
        _validate_official_inventory(manifest)
    if fetcher is None:
        fetcher = HttpRangeFetcher(manifest.source)
    central_bytes = (
        manifest.source.content_length - manifest.source.central_directory_offset
    )
    if central_bytes > config.max_network_bytes:
        raise MastrSupplementError(
            f"central tail exceeds network byte ceiling: "
            f"{central_bytes} > {config.max_network_bytes}"
        )
    central_evidence = verify_remote_manifest(manifest, fetcher)

    try:
        pinned_builder = bnetza._open_pinned(
            config.builder_csv,
            bnetza.Pin(config.builder_pin.sha256, config.builder_pin.bytes),
            "accepted builder",
        )
    except bnetza.BNetzASupplementError as exc:
        raise MastrSupplementError(str(exc)) from exc
    store: ProjectionStore | None = None
    try:
        store = ProjectionStore(
            config.sqlite_path,
            max_bytes=config.max_database_bytes,
            max_candidates=config.max_candidates,
            minimum_free_bytes=config.minimum_free_bytes,
        )
        member_evidence, source_counts = acquire(
            manifest,
            fetcher,
            store,
            max_address_members=config.max_address_members,
            max_total_compressed_bytes=config.max_total_compressed_bytes,
            max_total_uncompressed_bytes=config.max_total_uncompressed_bytes,
            max_network_bytes=config.max_network_bytes,
            initial_network_bytes=central_evidence["bytes"],
            max_member_compressed_bytes=config.max_member_compressed_bytes,
            max_member_uncompressed_bytes=config.max_member_uncompressed_bytes,
            max_member_attempts=config.max_member_attempts,
        )
        localities, relations, base_counts = _load_base_context(
            pinned_builder,
            store,
            expected_rows=config.expected_builder_rows,
            max_localities=config.max_localities,
            max_locality_postcode_relations=(
                config.max_locality_postcode_relations
            ),
            bloom_bytes=config.bloom_bytes,
            check_every_rows=config.base_check_rows,
        )
        try:
            bnetza._recheck_pinned(pinned_builder, "accepted builder")
        except bnetza.BNetzASupplementError as exc:
            raise MastrSupplementError(str(exc)) from exc
        resolution_counts = _resolve(store, localities, relations)

        pinned_builder.stream.seek(0)
        with builder._canonical_gzip_writer(
            config.output_csv, builder.BUILDER_HEADER
        ) as writer:
            guarded_writer = GuardedCsvWriter(
                writer,
                config.output_csv,
                max_bytes=config.max_output_bytes,
                minimum_free_bytes=config.minimum_free_bytes,
            )
            try:
                merge_counts = bnetza.merge_output(
                    pinned_builder,
                    store.iter_resolved(),
                    guarded_writer,
                    overlay_mode=bnetza.OVERLAY_ADD_AND_FILL,
                )
            except bnetza.BNetzASupplementError as exc:
                raise MastrSupplementError(str(exc)) from exc
            guarded_writer.check()
        guarded_writer.check()
        try:
            bnetza._recheck_pinned(pinned_builder, "accepted builder")
        except bnetza.BNetzASupplementError as exc:
            raise MastrSupplementError(str(exc)) from exc
        forbidden = (
            "base_blank_postcode_rows_filled",
            "source_rows_consumed_by_base_identity",
            "source_rows_already_present",
            "source_rows_quarantined_postcode_conflict",
            "source_only_rows_skipped_by_policy",
        )
        if any(merge_counts.get(name, 0) for name in forbidden):
            raise MastrSupplementError("source-only merge intersected an accepted identity")
        additions = resolution_counts["source_only_rows_resolved"]
        if merge_counts.get("base_rows_retained", 0) != config.expected_builder_rows:
            raise MastrSupplementError("base row conservation failed")
        if merge_counts.get("source_rows_added", 0) != additions:
            raise MastrSupplementError("source addition conservation failed")
        if merge_counts.get("output_rows", 0) != config.expected_builder_rows + additions:
            raise MastrSupplementError("output row conservation failed")
        store.check_bound()
        counts = Counter()
        counts.update(source_counts)
        counts.update(base_counts)
        counts.update(resolution_counts)
        counts.update(merge_counts)
        report: dict[str, Any] = {
            "schema": SCHEMA,
            "status": STATUS,
            "source": {
                **_source_json(manifest.source),
                "license": LICENSE,
                "license_name": LICENSE_NAME,
                "license_url": LICENSE_URL,
                "attribution": ATTRIBUTION,
                "changes_marked": True,
            },
            "policy": {
                "http_status_exact_206": True,
                "response_headers_pinned": True,
                "member_ranges_sequential": True,
                "member_savepoint_and_exact_retry": True,
                "archive_saved_to_disk": False,
                "streaming_raw_deflate": True,
                "streaming_utf16_xml": True,
                "strict_public_address_flags": True,
                "checked_active_in_operation_only": True,
                "complete_product_locality_relation": True,
                "equal_observations_collapsed": True,
                "conflicting_projection_fail_closed": True,
                "existing_base_identity_changed": False,
                "base_rows_order_and_coordinates_preserved": True,
                "source_only_additions": True,
                "postcode_fill_applied": False,
                "sqlite_candidate_and_base_relations_reusable_offline": True,
                "first_member_selection": False,
                "evaluation_inputs_read": False,
            },
            "limits": {
                "max_database_bytes": config.max_database_bytes,
                "max_output_bytes": config.max_output_bytes,
                "max_candidates": config.max_candidates,
                "max_localities": config.max_localities,
                "max_locality_postcode_relations": (
                    config.max_locality_postcode_relations
                ),
                "bloom_bytes": config.bloom_bytes,
                "max_address_members": config.max_address_members,
                "max_total_compressed_bytes": config.max_total_compressed_bytes,
                "max_total_uncompressed_bytes": (
                    config.max_total_uncompressed_bytes
                ),
                "max_network_bytes": config.max_network_bytes,
                "max_member_compressed_bytes": config.max_member_compressed_bytes,
                "max_member_uncompressed_bytes": config.max_member_uncompressed_bytes,
                "max_member_attempts": config.max_member_attempts,
                "base_check_rows": config.base_check_rows,
            },
            "inputs": {
                "manifest": manifest_input_evidence,
                "builder": dict(pinned_builder.evidence),
            },
            "members": member_evidence,
            "central_directory_verification": central_evidence,
            "counts": dict(sorted(counts.items())),
            "aggregation": _file_evidence(config.sqlite_path),
            "output": _file_evidence(config.output_csv),
        }
        with config.receipt.open("xb") as handle:
            handle.write(canonical_json_bytes(report))
            handle.flush()
            os.fsync(handle.fileno())
        return report
    finally:
        pinned_builder.stream.close()
        if store is not None:
            store.close()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-network", action="store_true")
    parser.add_argument("--write-official-manifest", type=Path)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--manifest-sha256")
    parser.add_argument("--manifest-bytes", type=int)
    parser.add_argument("--builder", type=Path)
    parser.add_argument("--builder-sha256")
    parser.add_argument("--builder-bytes", type=int)
    parser.add_argument("--expected-builder-rows", type=int)
    parser.add_argument("--sqlite", type=Path)
    parser.add_argument("--output-csv", type=Path)
    parser.add_argument("--receipt", type=Path)
    parser.add_argument("--minimum-free-bytes", type=int, default=DEFAULT_MIN_FREE_BYTES)
    parser.add_argument("--max-database-bytes", type=int, default=DEFAULT_MAX_DATABASE_BYTES)
    parser.add_argument("--max-output-bytes", type=int, default=DEFAULT_MAX_OUTPUT_BYTES)
    parser.add_argument("--max-network-bytes", type=int, default=DEFAULT_MAX_NETWORK_BYTES)
    parser.add_argument("--max-candidates", type=int, default=DEFAULT_MAX_CANDIDATES)
    parser.add_argument(
        "--max-member-attempts", type=int, default=DEFAULT_MAX_MEMBER_ATTEMPTS
    )
    parser.add_argument(
        "--max-locality-postcode-relations",
        type=int,
        default=DEFAULT_MAX_LOCALITY_POSTCODE_RELATIONS,
    )
    parser.add_argument(
        "--base-check-rows", type=int, default=DEFAULT_BASE_CHECK_ROWS
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    if not args.allow_network:
        raise SystemExit("MaStR acquisition requires explicit --allow-network")
    build_values = {
        "--manifest": args.manifest,
        "--manifest-sha256": args.manifest_sha256,
        "--manifest-bytes": args.manifest_bytes,
        "--builder": args.builder,
        "--builder-sha256": args.builder_sha256,
        "--builder-bytes": args.builder_bytes,
        "--expected-builder-rows": args.expected_builder_rows,
        "--sqlite": args.sqlite,
        "--output-csv": args.output_csv,
        "--receipt": args.receipt,
    }
    if args.write_official_manifest is not None:
        supplied = [name for name, value in build_values.items() if value is not None]
        if supplied:
            raise SystemExit(
                "manifest creation cannot be combined with build inputs: "
                + ", ".join(supplied)
            )
        try:
            report = write_official_manifest(args.write_official_manifest)
        except MastrSupplementError as exc:
            raise SystemExit(f"DE MaStR manifest creation refused: {exc}") from exc
        print(json.dumps(report, ensure_ascii=False, sort_keys=True))
        return 0
    missing = [name for name, value in build_values.items() if value is None]
    if missing:
        raise SystemExit("missing required build inputs: " + ", ".join(missing))
    config = Config(
        manifest=args.manifest,
        manifest_pin=Pin(args.manifest_sha256, args.manifest_bytes),
        builder_csv=args.builder,
        builder_pin=Pin(args.builder_sha256, args.builder_bytes),
        expected_builder_rows=args.expected_builder_rows,
        sqlite_path=args.sqlite,
        output_csv=args.output_csv,
        receipt=args.receipt,
        minimum_free_bytes=args.minimum_free_bytes,
        max_database_bytes=args.max_database_bytes,
        max_output_bytes=args.max_output_bytes,
        max_network_bytes=args.max_network_bytes,
        max_candidates=args.max_candidates,
        max_member_attempts=args.max_member_attempts,
        max_locality_postcode_relations=args.max_locality_postcode_relations,
        base_check_rows=args.base_check_rows,
    )
    try:
        report = build(config)
    except MastrSupplementError as exc:
        raise SystemExit(f"DE MaStR strict source-only supplement refused: {exc}") from exc
    print(json.dumps(report, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
