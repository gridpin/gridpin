#!/usr/bin/env python3
"""Create the only accepted LAB-only manifest for the Germany Photon/OSM sheet.

The merged builder CSV is ODbL-covered and is intentionally not a public DE
release input.  This module binds the sheet manifest to the canonical merge
receipt, the current public-DE source witness, and the exact pinned Photon
dump.  It never builds an index and never opens the network.
"""
from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import pathlib
import stat
import uuid
from collections.abc import Mapping, Sequence
from typing import Any

import de_photon_osm_supplement as supplement
import de_sources


CODE = pathlib.Path(__file__).resolve().parent.parent
WORKSPACE = CODE.parent

SCHEMA = "gridpin-de-photon-osm-lab-sheet-manifest-v1"
STATUS = supplement.LAB_STATUS
LICENSE = "ODbL-1.0"
LICENSE_URL = supplement.ODBL_LICENSE_URL
ATTRIBUTION = supplement.OSM_ATTRIBUTION
ATTRIBUTION_URL = supplement.OSM_ATTRIBUTION_URL
SHIPPING = "FORBIDDEN_WITHOUT_SEPARATE_OWNER_LEGAL_RELEASE_DECISION"
SOURCES = (
    "Overture Maps Addresses Germany + OpenStreetMap addresses via pinned "
    "Photon Germany dump"
)
SOURCE_RELEASE = (
    "overture-addresses=2026-08-19.0;photon-database=1.0.0-4;"
    "photon-data=2026-08-22T23:04:06.000+00:00"
)


class ContractError(RuntimeError):
    """The LAB manifest cannot be produced without weakening its contract."""


@dataclasses.dataclass(frozen=True)
class FilePin:
    path: str
    sha256: str
    bytes: int
    mode: str
    nlink: int = 1

    def evidence(self) -> dict[str, Any]:
        return {
            "path": self.path,
            "sha256": self.sha256,
            "bytes": self.bytes,
            "mode": self.mode,
            "nlink": self.nlink,
        }


@dataclasses.dataclass(frozen=True)
class ContractPins:
    base_manifest: FilePin
    base_stats: FilePin
    base_builder_csv: FilePin
    photon_dump: FilePin
    photon_jsonl_records: int
    photon_uncompressed_bytes: int
    base_source_catalog_sha256: str
    photon_version: str
    photon_database_version: str
    photon_data_timestamp: str


DEFAULT_PINS = ContractPins(
    base_manifest=FilePin(
        "code/data/de_manifest.json",
        "282515fb481ef8c49337fc5484de0e1961d588f8ae0c5a68532db0a76cadccad",
        20_290,
        "0644",
    ),
    base_stats=FilePin(
        "code/data/de_stats.json",
        "a961562811c3d98428b545626ad3bf54413ca8f2d2ae3f54e17b1213b28710a5",
        25_473,
        "0644",
    ),
    base_builder_csv=FilePin(
        "code/data/build_de.csv.gz",
        "f111c3f6ccd498a3237d3d01602c8fafc949d31253e1e5a07c79de9443db4098",
        247_594_238,
        "0644",
    ),
    photon_dump=FilePin(
        "code/eval/work/de_5k_comparison_photon_germany_release_260823_v1/"
        "photon-dump-germany-release-260823.jsonl.zst",
        "2a90f64d8643749bc16f121092d9e133978d692112976325ad7e589367e7077d",
        2_332_562_247,
        "0600",
    ),
    photon_jsonl_records=27_339_202,
    photon_uncompressed_bytes=71_311_428_222,
    base_source_catalog_sha256=(
        "ed12c1cb7f144a432226fdcc667924aacb98f0339a0bd6f65ae1037226de3cf4"
    ),
    photon_version="0.1.0",
    photon_database_version="1.0.0-4",
    photon_data_timestamp="2026-08-22T23:04:06.000+00:00",
)

_MANIFEST_KEYS = {
    "schema",
    "status",
    "country",
    "layer",
    "license",
    "license_url",
    "attribution",
    "attribution_url",
    "shipping",
    "sources",
    "source_release",
    "provenance",
}
_RECEIPT_KEYS = {
    "schema",
    "status",
    "license",
    "policy",
    "inputs",
    "photon_metadata",
    "configuration",
    "counts",
    "output",
}
_EVIDENCE_KEYS = {"path", "bytes", "sha256", "mode", "nlink"}
_HEX64 = set("0123456789abcdef")


def _canonical_json(value: Mapping[str, Any]) -> bytes:
    return supplement.canonical_json_bytes(value)


def _validate_sha256(value: object, label: str) -> str:
    text = str(value)
    if len(text) != 64 or any(char not in _HEX64 for char in text):
        raise ContractError(f"{label} must be exactly 64 lowercase hex characters")
    return text


def _mode(info: os.stat_result) -> str:
    return f"{stat.S_IMODE(info.st_mode):04o}"


def _file_bytes(path: pathlib.Path, label: str) -> tuple[bytes, os.stat_result]:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags)
    except OSError as exc:
        raise ContractError(f"cannot open {label}: {path}: {exc}") from exc
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise ContractError(f"{label} must be a regular nlink=1 file")
        chunks = []
        while block := os.read(fd, 1024 * 1024):
            chunks.append(block)
        named = os.stat(path, follow_symlinks=False)
        if (before.st_dev, before.st_ino) != (named.st_dev, named.st_ino):
            raise ContractError(f"{label} pathname changed while reading")
        return b"".join(chunks), before
    finally:
        os.close(fd)


def _stream_evidence(path: pathlib.Path, label: str) -> dict[str, Any]:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags)
    except OSError as exc:
        raise ContractError(f"cannot open {label}: {path}: {exc}") from exc
    digest = hashlib.sha256()
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise ContractError(f"{label} must be a regular nlink=1 file")
        while block := os.read(fd, 8 * 1024 * 1024):
            digest.update(block)
        named = os.stat(path, follow_symlinks=False)
        if (before.st_dev, before.st_ino) != (named.st_dev, named.st_ino):
            raise ContractError(f"{label} pathname changed while hashing")
        return {
            "bytes": before.st_size,
            "sha256": digest.hexdigest(),
            "mode": _mode(before),
            "nlink": before.st_nlink,
        }
    finally:
        os.close(fd)


def _resolve_contract_path(path: str, code_root: pathlib.Path) -> pathlib.Path:
    raw = pathlib.Path(path)
    if raw.is_absolute():
        return raw
    if raw.parts and raw.parts[0] == "code":
        return code_root.parent.joinpath(*raw.parts)
    return code_root / raw


def _display_path(path: pathlib.Path, workspace_root: pathlib.Path) -> str:
    try:
        return str(path.resolve(strict=False).relative_to(workspace_root.resolve()))
    except ValueError as exc:
        raise ContractError(f"LAB contract path escapes the workspace: {path}") from exc


def _load_json(raw: bytes, label: str) -> dict[str, Any]:
    try:
        value = supplement.strict_json_loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError, json.JSONDecodeError) as exc:
        raise ContractError(f"{label} is not strict UTF-8 JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise ContractError(f"{label} must be a JSON object")
    return value


def _verify_small_pin(
    pin: FilePin, label: str, *, code_root: pathlib.Path
) -> tuple[dict[str, Any], dict[str, Any]]:
    path = _resolve_contract_path(pin.path, code_root)
    raw, info = _file_bytes(path, label)
    actual = {
        "path": pin.path,
        "bytes": len(raw),
        "sha256": hashlib.sha256(raw).hexdigest(),
        "mode": _mode(info),
        "nlink": info.st_nlink,
    }
    if actual != pin.evidence():
        raise ContractError(f"{label} evidence differs from the frozen contract")
    return _load_json(raw, label), actual


def _receipt_path(raw: object, code_root: pathlib.Path) -> pathlib.Path:
    if not isinstance(raw, str) or not raw:
        raise ContractError("receipt evidence path must be a non-empty string")
    return _resolve_contract_path(raw, code_root)


def _evidence_matches(
    actual: object, expected: FilePin, label: str, *, code_root: pathlib.Path
) -> None:
    if not isinstance(actual, dict) or set(actual) != _EVIDENCE_KEYS:
        raise ContractError(f"{label} evidence schema drift")
    if _receipt_path(actual["path"], code_root).resolve(strict=False) != (
        _resolve_contract_path(expected.path, code_root).resolve(strict=False)
    ):
        raise ContractError(f"{label} path differs from the frozen contract")
    for key in ("bytes", "sha256", "mode", "nlink"):
        if actual.get(key) != expected.evidence()[key]:
            raise ContractError(f"{label} {key} differs from the frozen contract")


def _validate_receipt(
    receipt: dict[str, Any], pins: ContractPins, *, code_root: pathlib.Path
) -> None:
    if set(receipt) != _RECEIPT_KEYS:
        raise ContractError("merge receipt top-level schema drift")
    if receipt.get("schema") != supplement.SCHEMA or receipt.get("status") != STATUS:
        raise ContractError("merge receipt schema/status is not the accepted LAB supplement")
    expected_license = {
        "data": supplement.ODBL_LICENSE,
        "data_url": LICENSE_URL,
        "attribution": ATTRIBUTION,
        "attribution_url": ATTRIBUTION_URL,
        "shipping": SHIPPING,
    }
    if receipt.get("license") != expected_license:
        raise ContractError("merge receipt ODbL/attribution/shipping drift")
    expected_policy = {
        "network_calls": 0,
        "photon_engine_calls": 0,
        "gridpin_engine_calls": 0,
        "full_uncompressed_dump_materialized": False,
        "overture_nonblank_precedence": True,
        "blank_postcode_fill_requires_osm_consensus": True,
    }
    if receipt.get("policy") != expected_policy:
        raise ContractError("merge receipt policy drift")
    inputs = receipt.get("inputs")
    if not isinstance(inputs, dict) or set(inputs) != {
        "photon_dump",
        "overture_builder_csv",
    }:
        raise ContractError("merge receipt input schema drift")
    _evidence_matches(
        inputs["photon_dump"], pins.photon_dump, "Photon dump", code_root=code_root
    )
    _evidence_matches(
        inputs["overture_builder_csv"],
        pins.base_builder_csv,
        "base builder CSV",
        code_root=code_root,
    )
    expected_metadata = {
        "data_timestamp": pins.photon_data_timestamp,
        "database_version": pins.photon_database_version,
        "features": {
            "has_addresslines": False,
            "sorted_by_country": True,
        },
        "generator": "photon",
        "version": pins.photon_version,
    }
    if receipt.get("photon_metadata") != expected_metadata:
        raise ContractError("merge receipt Photon metadata drift")
    for key in ("configuration", "counts"):
        if not isinstance(receipt.get(key), dict):
            raise ContractError(f"merge receipt {key} must be an object")


def expected_manifest(
    receipt_path: pathlib.Path,
    *,
    pins: ContractPins = DEFAULT_PINS,
    code_root: pathlib.Path = CODE,
    workspace_root: pathlib.Path = WORKSPACE,
) -> dict[str, Any]:
    """Validate the merge lineage and return the exact manifest object."""

    base_manifest, base_manifest_evidence = _verify_small_pin(
        pins.base_manifest, "base DE manifest", code_root=code_root
    )
    try:
        de_sources.validate_manifest(base_manifest, de_sources.DE_RELEASE)
    except ValueError as exc:
        raise ContractError(f"base DE manifest is not canonical: {exc}") from exc
    base_stats, base_stats_evidence = _verify_small_pin(
        pins.base_stats, "base DE stats", code_root=code_root
    )
    stats_problems = de_sources.stats_witness_problems(
        base_stats, de_sources.DE_RELEASE
    )
    if stats_problems:
        raise ContractError("base DE stats witness invalid: " + "; ".join(stats_problems))
    if base_stats.get("source_catalog_sha256") != pins.base_source_catalog_sha256:
        raise ContractError("base DE stats source_catalog_sha256 drift")

    receipt_raw, receipt_info = _file_bytes(receipt_path, "merge receipt")
    if _mode(receipt_info) != "0600":
        raise ContractError("merge receipt must have exact mode 0600")
    receipt = _load_json(receipt_raw, "merge receipt")
    if _canonical_json(receipt) != receipt_raw:
        raise ContractError("merge receipt bytes are not canonical JSON")
    _validate_receipt(receipt, pins, code_root=code_root)

    output = receipt.get("output")
    if not isinstance(output, dict) or set(output) != _EVIDENCE_KEYS:
        raise ContractError("merge receipt output evidence schema drift")
    build_input = _receipt_path(output["path"], code_root)
    forbidden = code_root / "data" / "de.bin"
    if build_input.resolve(strict=False) == forbidden.resolve(strict=False):
        raise ContractError("LAB build input/output must never be code/data/de.bin")
    if not build_input.resolve(strict=False).is_relative_to(
        (code_root / "eval" / "work").resolve()
    ):
        raise ContractError("LAB builder CSV must stay under code/eval/work")
    actual_output = _stream_evidence(build_input, "LAB builder CSV")
    for key in ("bytes", "sha256", "mode", "nlink"):
        if output.get(key) != actual_output[key]:
            raise ContractError(f"LAB builder CSV {key} differs from merge receipt")
    if actual_output["mode"] != "0600" or actual_output["nlink"] != 1:
        raise ContractError("LAB builder CSV must have exact mode0600/nlink1")

    receipt_evidence = {
        "path": _display_path(receipt_path, workspace_root),
        "sha256": hashlib.sha256(receipt_raw).hexdigest(),
        "bytes": len(receipt_raw),
        "mode": _mode(receipt_info),
        "nlink": receipt_info.st_nlink,
    }
    build_input_evidence = {
        "path": _display_path(build_input, workspace_root),
        **actual_output,
    }
    photon = {
        **pins.photon_dump.evidence(),
        "jsonl_records": pins.photon_jsonl_records,
        "uncompressed_bytes": pins.photon_uncompressed_bytes,
        "version": pins.photon_version,
        "database_version": pins.photon_database_version,
        "data_timestamp": pins.photon_data_timestamp,
    }
    return {
        "schema": SCHEMA,
        "status": STATUS,
        "country": "de",
        "layer": "addresses",
        "license": LICENSE,
        "license_url": LICENSE_URL,
        "attribution": ATTRIBUTION,
        "attribution_url": ATTRIBUTION_URL,
        "shipping": SHIPPING,
        "sources": SOURCES,
        "source_release": SOURCE_RELEASE,
        "provenance": {
            "base_manifest": base_manifest_evidence,
            "base_stats": base_stats_evidence,
            "base_source_catalog_sha256": pins.base_source_catalog_sha256,
            "base_builder_csv": pins.base_builder_csv.evidence(),
            "photon_dump": photon,
            "merge_receipt": receipt_evidence,
            "build_input": build_input_evidence,
        },
    }


def validate_manifest(
    manifest: object,
    receipt_path: pathlib.Path,
    *,
    pins: ContractPins = DEFAULT_PINS,
    code_root: pathlib.Path = CODE,
    workspace_root: pathlib.Path = WORKSPACE,
) -> None:
    if not isinstance(manifest, dict) or set(manifest) != _MANIFEST_KEYS:
        raise ContractError("LAB manifest exact top-level schema drift")
    expected = expected_manifest(
        receipt_path,
        pins=pins,
        code_root=code_root,
        workspace_root=workspace_root,
    )
    if _canonical_json(manifest) != _canonical_json(expected):
        raise ContractError("LAB manifest differs from receipt-derived canonical value")


def load_manifest(path: pathlib.Path) -> dict[str, Any]:
    raw, info = _file_bytes(path, "LAB manifest")
    if _mode(info) != "0600":
        raise ContractError("LAB manifest must have exact mode 0600")
    value = _load_json(raw, "LAB manifest")
    if _canonical_json(value) != raw:
        raise ContractError("LAB manifest bytes are not canonical JSON")
    return value


def _fsync_directory(path: pathlib.Path) -> None:
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def write_manifest(
    destination: pathlib.Path,
    receipt_path: pathlib.Path,
    *,
    pins: ContractPins = DEFAULT_PINS,
    code_root: pathlib.Path = CODE,
    workspace_root: pathlib.Path = WORKSPACE,
) -> dict[str, Any]:
    destination = pathlib.Path(destination)
    if destination.resolve(strict=False) == (code_root / "data" / "de.bin").resolve(
        strict=False
    ):
        raise ContractError("refusing to write a LAB manifest to code/data/de.bin")
    work_root = (code_root / "eval" / "work").resolve()
    if not destination.resolve(strict=False).is_relative_to(work_root):
        raise ContractError("LAB manifest must stay under code/eval/work")
    if not destination.parent.is_dir() or destination.parent.is_symlink():
        raise ContractError("LAB manifest parent must be an existing real directory")
    manifest = expected_manifest(
        receipt_path,
        pins=pins,
        code_root=code_root,
        workspace_root=workspace_root,
    )
    payload = _canonical_json(manifest)
    temporary = destination.parent / f".{destination.name}.{uuid.uuid4().hex}.partial"
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    fd = -1
    try:
        fd = os.open(temporary, flags, 0o600)
        os.fchmod(fd, 0o600)
        offset = 0
        while offset < len(payload):
            offset += os.write(fd, payload[offset:])
        os.fsync(fd)
        os.close(fd)
        fd = -1
        try:
            os.link(temporary, destination, follow_symlinks=False)
        except FileExistsError as exc:
            raise ContractError(f"refusing to overwrite existing manifest: {destination}") from exc
        _fsync_directory(destination.parent)
        temporary.unlink()
        _fsync_directory(destination.parent)
    finally:
        if fd >= 0:
            os.close(fd)
        if temporary.exists():
            temporary.unlink()
    info = os.stat(destination, follow_symlinks=False)
    if not stat.S_ISREG(info.st_mode) or _mode(info) != "0600" or info.st_nlink != 1:
        raise ContractError("published LAB manifest is not regular mode0600/nlink1")
    return manifest


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--merge-receipt", type=pathlib.Path, required=True)
    parser.add_argument("--output-manifest", type=pathlib.Path, required=True)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        manifest = write_manifest(args.output_manifest, args.merge_receipt)
    except ContractError as exc:
        print(f"STOP: {exc}", file=os.sys.stderr)
        return 2
    print(_canonical_json(manifest).decode("utf-8"), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
