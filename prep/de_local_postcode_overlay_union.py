#!/usr/bin/env python3
"""Build the exhaustive, outcome-blind union of four local DE postcode overlays.

The five input builders are streamed in lockstep: the accepted BNetzA
postcode-fill builder is the immutable base and KiBiz, Schulgrunddaten NRW,
ISIL/lobid, and EEA IED are proposal-only overlays.  A blank base postcode is
filled only when every nonblank proposal is the same normalized five-digit
value.  Conflicts stay blank; source order never breaks a tie.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
from collections.abc import Iterator, Mapping, Sequence
from contextlib import ExitStack, contextmanager
import csv
from dataclasses import dataclass
import gzip
import hashlib
import io
from itertools import zip_longest
import json
import os
from pathlib import Path
import re
import shutil
import stat
from typing import Any, BinaryIO, TextIO


SCHEMA = "gridpin-de-local-postcode-overlay-union-v1"
STATUS = "PUBLIC_PERMISSIVE_DEVELOPMENT"
PREREGISTRATION_SCHEMA = (
    "gridpin-de-continuous-local-postcode-union-preregistration-v1"
)
PREREGISTRATION_STATUS = "PREREGISTERED_BEFORE_UNION_BUILD"
EXPECTED_ROWS = 19_267_049
EXPECTED_ANY = 13_179
EXPECTED_FILLABLE = 13_170
EXPECTED_MULTI_SOURCE_AGREEMENT = 76
EXPECTED_CONFLICTS = 9
DEFAULT_MINIMUM_FREE_BYTES = 5 * 2**30
OUTPUT_RESERVE_BYTES = 512 * 2**20
RECEIPT_RESERVE_BYTES = 1 * 2**20

BUILDER_HEADER = (
    "nom_voie_norm",
    "code_insee",
    "nom_commune_norm",
    "code_postal",
    "code_postal_display",
    "numero",
    "rep",
    "lon",
    "lat",
    "nom_voie",
    "nom_commune",
    "provincia_norm",
)
POSTCODE_FIELDS = frozenset(("code_postal", "code_postal_display"))
NON_POSTCODE_FIELDS = tuple(name for name in BUILDER_HEADER if name not in POSTCODE_FIELDS)
OVERLAY_NAMES = (
    "kibiz",
    "schulgrunddaten_nrw",
    "isil_lobid",
    "eea_ied",
)
EXPECTED_EXCLUSIVE = {
    "kibiz": 6_419,
    "schulgrunddaten_nrw": 1_962,
    "isil_lobid": 2_061,
    "eea_ied": 2_652,
}
EXPECTED_RECEIPT_SCHEMAS = {
    "baseline": "gridpin-de-bnetza-address-supplement-v1",
    "kibiz": "gridpin-de-kibiz-postcode-overlay-v1",
    "schulgrunddaten_nrw": "gridpin-de-schulgrunddaten-nrw-postcode-overlay-v1",
    "isil_lobid": "gridpin-de-isil-lobid-postcode-overlay-v1",
    "eea_ied": "gridpin-de-eea-ied-postcode-overlay-v1",
}
PRODUCTION_EXPECTED = {
    "rows": EXPECTED_ROWS,
    "rows_with_any_proposal": EXPECTED_ANY,
    "fillable_rows": EXPECTED_FILLABLE,
    "multi_source_agreement_rows": EXPECTED_MULTI_SOURCE_AGREEMENT,
    "conflicting_rows": EXPECTED_CONFLICTS,
    "exclusive_proposal_rows": EXPECTED_EXCLUSIVE,
}

_POSTCODE = re.compile(r"[0-9]{5}")
_SHA256 = re.compile(r"[0-9a-f]{64}")
_SENTINEL = object()
REPOSITORY_ROOT = Path(__file__).resolve().parents[2]


class UnionError(RuntimeError):
    """The union cannot be produced without weakening a frozen guard."""


@dataclass(frozen=True)
class Pin:
    sha256: str
    bytes: int


@dataclass
class PinnedFile:
    path: Path
    stream: BinaryIO
    pin: Pin
    device: int
    inode: int
    evidence: dict[str, Any]


@dataclass(frozen=True)
class BuilderSpec:
    name: str
    builder_csv: Path
    builder_pin: Pin
    merge_receipt: Path
    receipt_pin: Pin
    expected_receipt_schema: str


@dataclass(frozen=True)
class Contract:
    baseline: BuilderSpec
    overlays: tuple[BuilderSpec, ...]
    expected: Mapping[str, Any]
    output_csv: Path
    output_receipt: Path


@dataclass(frozen=True)
class Decision:
    postcode: str
    proposal_sources: tuple[str, ...]
    distinct_values: int


@dataclass
class CreatedGzipOutput:
    """Create-only gzip writer whose original inode stays pinned until receipt."""

    path: Path
    raw: BinaryIO
    binary: gzip.GzipFile
    text: TextIO
    writer: csv.DictWriter
    proof: BinaryIO
    device: int
    inode: int
    evidence: dict[str, Any] | None = None
    finalized: bool = False

    @classmethod
    def create(cls, path: Path) -> "CreatedGzipOutput":
        flags = (
            os.O_RDWR
            | os.O_CREAT
            | os.O_EXCL
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_NOFOLLOW", 0)
        )
        try:
            descriptor = os.open(path, flags, 0o600)
        except OSError as exc:
            raise UnionError(f"cannot create output CSV: {exc}") from exc
        proof_descriptor: int | None = None
        proof: BinaryIO | None = None
        raw: BinaryIO | None = None
        try:
            info = os.fstat(descriptor)
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                raise UnionError("new output CSV is not a one-link regular file")
            proof_descriptor = os.dup(descriptor)
            proof = os.fdopen(proof_descriptor, "rb", buffering=0)
            proof_descriptor = None
            raw = os.fdopen(descriptor, "wb", buffering=0)
            binary = gzip.GzipFile(
                filename="", mode="wb", fileobj=raw, compresslevel=6, mtime=0
            )
            text = io.TextIOWrapper(
                binary, encoding="utf-8", errors="strict", newline=""
            )
            writer = csv.DictWriter(
                text,
                fieldnames=BUILDER_HEADER,
                extrasaction="raise",
                lineterminator="\n",
            )
            writer.writeheader()
            return cls(
                path=path,
                raw=raw,
                binary=binary,
                text=text,
                writer=writer,
                proof=proof,
                device=info.st_dev,
                inode=info.st_ino,
            )
        except Exception:
            if raw is not None:
                raw.close()
            else:
                os.close(descriptor)
            if proof_descriptor is not None:
                os.close(proof_descriptor)
            if proof is not None:
                proof.close()
            raise

    def _identity_check(self) -> os.stat_result:
        info = os.fstat(self.proof.fileno())
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_nlink != 1
            or (info.st_dev, info.st_ino) != (self.device, self.inode)
        ):
            raise UnionError("created output CSV inode or link count changed")
        try:
            path_info = self.path.stat(follow_symlinks=False)
        except OSError as exc:
            raise UnionError("created output CSV path disappeared") from exc
        if (
            not stat.S_ISREG(path_info.st_mode)
            or path_info.st_nlink != 1
            or (path_info.st_dev, path_info.st_ino) != (self.device, self.inode)
        ):
            raise UnionError("created output CSV path was replaced")
        return info

    def finalize(self) -> dict[str, Any]:
        if self.finalized:
            assert self.evidence is not None
            return dict(self.evidence)
        self.text.flush()
        self.text.detach()
        self.binary.close()
        self.raw.flush()
        os.fsync(self.raw.fileno())
        self.raw.close()
        info = self._identity_check()
        digest, size = _hash_stream(self.proof)
        self.evidence = {
            "path": _display_path(self.path),
            "bytes": size,
            "sha256": digest,
            "mode": f"{stat.S_IMODE(info.st_mode):04o}",
            "nlink": info.st_nlink,
        }
        self.finalized = True
        return dict(self.evidence)

    def recheck(self) -> None:
        if not self.finalized or self.evidence is None:
            raise UnionError("created output CSV was not finalized")
        self._identity_check()
        digest, size = _hash_stream(self.proof)
        if digest != self.evidence["sha256"] or size != self.evidence["bytes"]:
            raise UnionError("created output CSV contents changed before receipt completion")

    def close(self) -> None:
        if not self.finalized:
            try:
                self.text.close()
            except (OSError, ValueError):
                pass
            try:
                self.binary.close()
            except OSError:
                pass
            try:
                self.raw.close()
            except OSError:
                pass
        self.proof.close()


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


def _strict_json_loads(raw: str, label: str) -> Any:
    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in items:
            if key in result:
                raise UnionError(f"{label} contains duplicate JSON key {key!r}")
            result[key] = value
        return result

    def reject_constant(value: str) -> None:
        raise UnionError(f"{label} contains non-finite JSON number {value}")

    try:
        return json.loads(raw, object_pairs_hook=pairs, parse_constant=reject_constant)
    except UnionError:
        raise
    except (UnicodeError, ValueError, json.JSONDecodeError) as exc:
        raise UnionError(f"{label} is not strict JSON: {exc}") from exc


def _validate_pin(pin: Pin, label: str) -> None:
    if _SHA256.fullmatch(pin.sha256) is None:
        raise UnionError(f"{label} SHA-256 pin is invalid")
    if isinstance(pin.bytes, bool) or not isinstance(pin.bytes, int) or pin.bytes <= 0:
        raise UnionError(f"{label} byte pin must be a positive integer")


def _hash_stream(stream: BinaryIO) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    stream.seek(0)
    while block := stream.read(8 * 2**20):
        digest.update(block)
        size += len(block)
    stream.seek(0)
    return digest.hexdigest(), size


def _display_path(path: Path) -> str:
    resolved = path.resolve(strict=True)
    try:
        return str(resolved.relative_to(REPOSITORY_ROOT))
    except ValueError:
        return str(resolved)


def _open_pinned(path: Path, pin: Pin, label: str) -> PinnedFile:
    _validate_pin(pin, label)
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise UnionError(f"cannot open pinned {label}: {exc}") from exc
    stream: BinaryIO | None = None
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise UnionError(f"{label} must be a one-link regular file")
        stream = os.fdopen(descriptor, "rb", buffering=0)
        digest, size = _hash_stream(stream)
        if size != pin.bytes or digest != pin.sha256:
            raise UnionError(
                f"{label} pin mismatch: got {digest}/{size}, "
                f"expected {pin.sha256}/{pin.bytes}"
            )
        path_info = path.stat(follow_symlinks=False)
        if (path_info.st_dev, path_info.st_ino) != (info.st_dev, info.st_ino):
            raise UnionError(f"{label} path identity changed while opening")
        return PinnedFile(
            path=path,
            stream=stream,
            pin=pin,
            device=info.st_dev,
            inode=info.st_ino,
            evidence={
                "path": _display_path(path),
                "bytes": size,
                "sha256": digest,
                "mode": f"{stat.S_IMODE(info.st_mode):04o}",
                "nlink": info.st_nlink,
            },
        )
    except Exception:
        if stream is not None:
            stream.close()
        else:
            os.close(descriptor)
        raise


def _recheck_pinned(pinned: PinnedFile, label: str) -> None:
    info = os.fstat(pinned.stream.fileno())
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_nlink != 1
        or (info.st_dev, info.st_ino) != (pinned.device, pinned.inode)
    ):
        raise UnionError(f"{label} identity changed during union build")
    try:
        path_info = pinned.path.stat(follow_symlinks=False)
    except OSError as exc:
        raise UnionError(f"{label} path disappeared during union build") from exc
    if (path_info.st_dev, path_info.st_ino) != (pinned.device, pinned.inode):
        raise UnionError(f"{label} path was replaced during union build")
    digest, size = _hash_stream(pinned.stream)
    if size != pinned.pin.bytes or digest != pinned.pin.sha256:
        raise UnionError(f"{label} contents changed during union build")


def _read_json_object(pinned: PinnedFile, label: str) -> Mapping[str, Any]:
    pinned.stream.seek(0)
    try:
        raw = pinned.stream.read().decode("utf-8", errors="strict")
    except UnicodeDecodeError as exc:
        raise UnionError(f"{label} is not strict UTF-8") from exc
    value = _strict_json_loads(raw, label)
    if not isinstance(value, dict):
        raise UnionError(f"{label} must be a JSON object")
    return value


def _mapping(value: Any, label: str) -> Mapping[str, Any]:
    if not isinstance(value, dict):
        raise UnionError(f"{label} must be an object")
    return value


def _text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value or "\x00" in value:
        raise UnionError(f"{label} must be a nonempty string")
    return value


def _positive_int(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise UnionError(f"{label} must be a positive integer")
    return value


def _path(value: Any, label: str) -> Path:
    text = _text(value, label)
    candidate = Path(text)
    return candidate if candidate.is_absolute() else REPOSITORY_ROOT / candidate


def _pin_from_evidence(value: Any, label: str) -> Pin:
    evidence = _mapping(value, label)
    pin = Pin(
        sha256=_text(evidence.get("sha256"), f"{label}.sha256"),
        bytes=_positive_int(evidence.get("bytes"), f"{label}.bytes"),
    )
    _validate_pin(pin, label)
    return pin


def _builder_spec(name: str, value: Any) -> BuilderSpec:
    item = _mapping(value, f"inputs.{name}")
    builder_doc = _mapping(item.get("builder_csv"), f"inputs.{name}.builder_csv")
    receipt_doc = _mapping(
        item.get("merge_receipt"), f"inputs.{name}.merge_receipt"
    )
    schema = _text(
        item.get("expected_receipt_schema"),
        f"inputs.{name}.expected_receipt_schema",
    )
    expected_schema = EXPECTED_RECEIPT_SCHEMAS[name]
    if schema != expected_schema:
        raise UnionError(f"inputs.{name} receipt schema pin drift")
    return BuilderSpec(
        name=name,
        builder_csv=_path(builder_doc.get("path"), f"inputs.{name}.builder_csv.path"),
        builder_pin=_pin_from_evidence(builder_doc, f"inputs.{name}.builder_csv"),
        merge_receipt=_path(
            receipt_doc.get("path"), f"inputs.{name}.merge_receipt.path"
        ),
        receipt_pin=_pin_from_evidence(
            receipt_doc, f"inputs.{name}.merge_receipt"
        ),
        expected_receipt_schema=schema,
    )


def _normalize_expected(value: Any, label: str) -> dict[str, Any]:
    item = _mapping(value, label)
    exclusive = _mapping(item.get("exclusive_proposal_rows"), f"{label}.exclusive_proposal_rows")
    if set(exclusive) != set(OVERLAY_NAMES):
        raise UnionError(f"{label}.exclusive_proposal_rows keys drift")
    return {
        "rows": _positive_int(item.get("rows"), f"{label}.rows"),
        "rows_with_any_proposal": _positive_int(
            item.get("rows_with_any_proposal"), f"{label}.rows_with_any_proposal"
        ),
        "fillable_rows": _positive_int(
            item.get("fillable_rows"), f"{label}.fillable_rows"
        ),
        "multi_source_agreement_rows": _positive_int(
            item.get("multi_source_agreement_rows"),
            f"{label}.multi_source_agreement_rows",
        ),
        "conflicting_rows": _positive_int(
            item.get("conflicting_rows"), f"{label}.conflicting_rows"
        ),
        "exclusive_proposal_rows": {
            name: _positive_int(exclusive.get(name), f"{label}.exclusive_proposal_rows.{name}")
            for name in OVERLAY_NAMES
        },
    }


def _same_path(left: Path, right: Path, *, strict: bool) -> bool:
    return left.resolve(strict=strict) == right.resolve(strict=strict)


def _load_contract(
    preregistration: PinnedFile,
    *,
    output_csv: Path,
    output_receipt: Path,
    required_expected: Mapping[str, Any],
) -> Contract:
    value = _read_json_object(preregistration, "union preregistration")
    if value.get("schema") != PREREGISTRATION_SCHEMA:
        raise UnionError("union preregistration schema drift")
    if value.get("status") != PREREGISTRATION_STATUS:
        raise UnionError("union preregistration status drift")
    inputs = _mapping(value.get("inputs"), "inputs")
    baseline = _builder_spec("baseline", inputs.get("baseline"))
    overlay_values = _mapping(inputs.get("overlays"), "inputs.overlays")
    if set(overlay_values) != set(OVERLAY_NAMES):
        raise UnionError("preregistration must pin exactly the four allowed overlays")
    overlays = tuple(_builder_spec(name, overlay_values.get(name)) for name in OVERLAY_NAMES)
    expected = _normalize_expected(value.get("expected"), "expected")
    if expected != dict(required_expected):
        raise UnionError("preregistered exhaustive-union count pins drift")
    outputs = _mapping(value.get("outputs"), "outputs")
    if outputs.get("expected_receipt_schema") != SCHEMA:
        raise UnionError("preregistered output receipt schema drift")
    if outputs.get("expected_receipt_status") != STATUS:
        raise UnionError("preregistered output receipt status drift")
    if outputs.get("write_once") is not True:
        raise UnionError("preregistered output is not write-once")
    if outputs.get("archive_or_expanded_intermediate_forbidden") is not True:
        raise UnionError("preregistration permits a forbidden expanded intermediate")
    frozen_csv = _path(outputs.get("builder_csv_path"), "outputs.builder_csv_path")
    frozen_receipt = _path(outputs.get("receipt_path"), "outputs.receipt_path")
    if not _same_path(frozen_csv, output_csv, strict=False):
        raise UnionError("output CSV path differs from preregistration")
    if not _same_path(frozen_receipt, output_receipt, strict=False):
        raise UnionError("output receipt path differs from preregistration")
    return Contract(
        baseline=baseline,
        overlays=overlays,
        expected=expected,
        output_csv=output_csv,
        output_receipt=output_receipt,
    )


def _receipt_output_evidence(
    receipt: Mapping[str, Any], label: str
) -> Mapping[str, Any]:
    output = _mapping(receipt.get("output"), f"{label}.output")
    _text(output.get("path"), f"{label}.output.path")
    _pin_from_evidence(output, f"{label}.output")
    return output


def _validate_receipt(
    spec: BuilderSpec,
    receipt: Mapping[str, Any],
    *,
    baseline: BuilderSpec,
) -> Mapping[str, Any]:
    label = f"{spec.name} merge receipt"
    if receipt.get("schema") != spec.expected_receipt_schema:
        raise UnionError(f"{label} schema drift")
    if receipt.get("status") != STATUS:
        raise UnionError(f"{label} status drift")
    configuration = _mapping(receipt.get("configuration"), f"{label}.configuration")
    if configuration.get("builder_header") != list(BUILDER_HEADER):
        raise UnionError(f"{label} builder header pin drift")
    if configuration.get("expected_builder_rows") != EXPECTED_ROWS:
        raise UnionError(f"{label} expected row count drift")
    counts = _mapping(receipt.get("counts"), f"{label}.counts")
    if counts.get("output_rows") != EXPECTED_ROWS:
        raise UnionError(f"{label} output row count drift")
    output = _receipt_output_evidence(receipt, label)
    output_pin = _pin_from_evidence(output, f"{label}.output")
    if output_pin != spec.builder_pin:
        raise UnionError(f"{label} output pin differs from preregistration")
    if not _same_path(_path(output.get("path"), f"{label}.output.path"), spec.builder_csv, strict=True):
        raise UnionError(f"{label} output path differs from preregistration")
    license_doc = _mapping(receipt.get("license"), f"{label}.license")
    if not license_doc:
        raise UnionError(f"{label} license is empty")
    policy = _mapping(receipt.get("policy"), f"{label}.policy")
    if policy.get("network_calls_during_build") != 0:
        raise UnionError(f"{label} was not built offline")
    if policy.get("photon_engine_calls_during_build") != 0:
        raise UnionError(f"{label} used Photon")
    if spec.name != "baseline":
        source_builder = _mapping(
            _mapping(receipt.get("inputs"), f"{label}.inputs").get("builder_csv"),
            f"{label}.inputs.builder_csv",
        )
        if _pin_from_evidence(source_builder, f"{label}.inputs.builder_csv") != baseline.builder_pin:
            raise UnionError(f"{label} was not derived from the pinned baseline")
        if not _same_path(
            _path(source_builder.get("path"), f"{label}.inputs.builder_csv.path"),
            baseline.builder_csv,
            strict=True,
        ):
            raise UnionError(f"{label} baseline path drift")
    return license_doc


@contextmanager
def _csv_rows(pinned: PinnedFile, label: str) -> Iterator[csv.DictReader]:
    pinned.stream.seek(0)
    binary = gzip.GzipFile(fileobj=pinned.stream, mode="rb")
    text = io.TextIOWrapper(binary, encoding="utf-8", errors="strict", newline="")
    reader = csv.DictReader(text)
    try:
        if tuple(reader.fieldnames or ()) != BUILDER_HEADER:
            raise UnionError(f"{label} builder header drift")
        yield reader
    except (UnicodeError, csv.Error, EOFError, gzip.BadGzipFile) as exc:
        raise UnionError(f"{label} is not a valid strict builder CSV.gz: {exc}") from exc
    finally:
        try:
            text.detach()
        except (ValueError, OSError):
            pass
        binary.close()


def _postcode_pair(row: Mapping[str, str], label: str) -> str:
    numeric = row.get("code_postal")
    display = row.get("code_postal_display")
    if not isinstance(numeric, str) or not isinstance(display, str):
        raise UnionError(f"{label} is missing postcode columns")
    if numeric == "" and display == "":
        return ""
    if numeric != display or _POSTCODE.fullmatch(numeric) is None:
        raise UnionError(f"{label} postcode pair must be blank or equal five digits")
    return numeric


def _non_postcode_values(row: Mapping[str, str], label: str) -> tuple[str, ...]:
    if None in row or any(not isinstance(row.get(name), str) for name in BUILDER_HEADER):
        raise UnionError(f"{label} has too many, too few, or missing CSV cells")
    return tuple(row[name] for name in NON_POSTCODE_FIELDS)


def decide_postcode(
    base_row: Mapping[str, str],
    overlay_rows: Mapping[str, Mapping[str, str]],
) -> Decision:
    """Return the order-independent per-row postcode decision."""

    if set(overlay_rows) != set(OVERLAY_NAMES):
        raise UnionError("row decision requires exactly the four allowed overlays")
    base_non_postcode = _non_postcode_values(base_row, "baseline row")
    base_postcode = _postcode_pair(base_row, "baseline row")
    proposals: dict[str, str] = {}
    for name in OVERLAY_NAMES:
        row = overlay_rows[name]
        if _non_postcode_values(row, f"{name} row") != base_non_postcode:
            raise UnionError(f"{name} changed a non-postcode cell or row order")
        postcode = _postcode_pair(row, f"{name} row")
        if base_postcode:
            if postcode != base_postcode:
                raise UnionError(f"{name} changed a nonblank baseline postcode")
        elif postcode:
            proposals[name] = postcode
    if base_postcode:
        return Decision(base_postcode, (), 0)
    distinct = frozenset(proposals.values())
    if len(distinct) == 1:
        (selected,) = tuple(distinct)
        return Decision(selected, tuple(sorted(proposals)), 1)
    return Decision("", tuple(sorted(proposals)), len(distinct))


def _empty_counts() -> dict[str, Any]:
    return {
        "rows": 0,
        "base_nonblank_rows": 0,
        "base_blank_rows": 0,
        "rows_with_any_proposal": 0,
        "fillable_rows": 0,
        "multi_source_agreement_rows": 0,
        "conflicting_rows": 0,
        "rows_without_proposal": 0,
        "rows_left_blank": 0,
        "output_nonblank_rows": 0,
        "source_rows_added": 0,
        "exclusive_proposal_rows": {name: 0 for name in OVERLAY_NAMES},
        "proposal_rows_by_overlay": {name: 0 for name in OVERLAY_NAMES},
    }


def _stream_pass(
    baseline: PinnedFile,
    overlays: Mapping[str, PinnedFile],
    *,
    expected_rows: int,
    writer: csv.DictWriter | None = None,
) -> dict[str, Any]:
    if set(overlays) != set(OVERLAY_NAMES):
        raise UnionError("stream pass requires exactly four overlays")
    counts = _empty_counts()
    with ExitStack() as stack:
        base_reader = stack.enter_context(_csv_rows(baseline, "baseline builder"))
        overlay_readers = {
            name: stack.enter_context(_csv_rows(overlays[name], f"{name} builder"))
            for name in OVERLAY_NAMES
        }
        iterators = [base_reader, *(overlay_readers[name] for name in OVERLAY_NAMES)]
        for ordinal, row_group in enumerate(
            zip_longest(*iterators, fillvalue=_SENTINEL), start=1
        ):
            if any(row is _SENTINEL for row in row_group):
                raise UnionError(f"builder row-count mismatch at ordinal {ordinal}")
            base_row = row_group[0]
            assert isinstance(base_row, dict)
            overlay_rows = {
                name: row_group[index + 1]
                for index, name in enumerate(OVERLAY_NAMES)
            }
            if not all(isinstance(row, dict) for row in overlay_rows.values()):
                raise UnionError(f"invalid overlay row at ordinal {ordinal}")
            decision = decide_postcode(base_row, overlay_rows)  # type: ignore[arg-type]
            counts["rows"] += 1
            base_postcode = _postcode_pair(base_row, "baseline row")
            if base_postcode:
                counts["base_nonblank_rows"] += 1
            else:
                counts["base_blank_rows"] += 1
                for name in decision.proposal_sources:
                    counts["proposal_rows_by_overlay"][name] += 1
                if decision.proposal_sources:
                    counts["rows_with_any_proposal"] += 1
                    if len(decision.proposal_sources) == 1:
                        counts["exclusive_proposal_rows"][decision.proposal_sources[0]] += 1
                else:
                    counts["rows_without_proposal"] += 1
                if decision.distinct_values == 1:
                    counts["fillable_rows"] += 1
                    if len(decision.proposal_sources) >= 2:
                        counts["multi_source_agreement_rows"] += 1
                elif decision.distinct_values > 1:
                    counts["conflicting_rows"] += 1
            if decision.postcode:
                counts["output_nonblank_rows"] += 1
            else:
                counts["rows_left_blank"] += 1
            if writer is not None:
                output_row = dict(base_row)
                if not base_postcode and decision.postcode:
                    output_row["code_postal"] = decision.postcode
                    output_row["code_postal_display"] = decision.postcode
                writer.writerow(output_row)
    if counts["rows"] != expected_rows:
        raise UnionError(
            f"builder row-count pin mismatch: got {counts['rows']}, expected {expected_rows}"
        )
    _validate_count_conservation(counts)
    return counts


def _validate_count_conservation(counts: Mapping[str, Any]) -> None:
    rows = counts["rows"]
    if counts["base_nonblank_rows"] + counts["base_blank_rows"] != rows:
        raise UnionError("base blank/nonblank count conservation failed")
    if counts["rows_without_proposal"] + counts["rows_with_any_proposal"] != counts["base_blank_rows"]:
        raise UnionError("proposal coverage count conservation failed")
    if counts["fillable_rows"] + counts["conflicting_rows"] != counts["rows_with_any_proposal"]:
        raise UnionError("fillable/conflict count conservation failed")
    if counts["rows_left_blank"] + counts["output_nonblank_rows"] != rows:
        raise UnionError("output blank/nonblank count conservation failed")
    if counts["output_nonblank_rows"] != counts["base_nonblank_rows"] + counts["fillable_rows"]:
        raise UnionError("output nonblank semantic count conservation failed")
    if counts["rows_left_blank"] != counts["rows_without_proposal"] + counts["conflicting_rows"]:
        raise UnionError("left-blank semantic count conservation failed")
    if counts["source_rows_added"] != 0:
        raise UnionError("source-row count conservation failed")
    exclusive_total = sum(counts["exclusive_proposal_rows"].values())
    if exclusive_total + counts["multi_source_agreement_rows"] + counts["conflicting_rows"] != counts["rows_with_any_proposal"]:
        raise UnionError("exclusive/agreement/conflict count conservation failed")


def _assert_expected(counts: Mapping[str, Any], expected: Mapping[str, Any]) -> None:
    for key in (
        "rows",
        "rows_with_any_proposal",
        "fillable_rows",
        "multi_source_agreement_rows",
        "conflicting_rows",
    ):
        if counts.get(key) != expected.get(key):
            raise UnionError(
                f"union count pin mismatch at {key}: got {counts.get(key)}, "
                f"expected {expected.get(key)}"
            )
    if counts.get("exclusive_proposal_rows") != expected.get("exclusive_proposal_rows"):
        raise UnionError("union exclusive-proposal count pins mismatch")


def _preflight_outputs(contract: Contract) -> None:
    outputs = (contract.output_csv, contract.output_receipt)
    for path in outputs:
        if path.exists() or path.is_symlink():
            raise UnionError(f"refusing to overwrite output: {path}")
        if not path.parent.is_dir() or path.parent.is_symlink():
            raise UnionError(f"output parent must be an existing real directory: {path.parent}")
    input_paths = [contract.baseline.builder_csv, contract.baseline.merge_receipt]
    for spec in contract.overlays:
        input_paths.extend((spec.builder_csv, spec.merge_receipt))
    resolved = [path.resolve(strict=True) for path in input_paths]
    resolved.extend(path.resolve(strict=False) for path in outputs)
    if len(set(resolved)) != len(resolved):
        raise UnionError("all input and output paths must be distinct")


def _require_free_bytes(path: Path, required: int, label: str) -> None:
    free = shutil.disk_usage(path).free
    if free < required:
        raise UnionError(
            f"disk floor/reserve crossed at {label}: got {free}, required {required}"
        )


@contextmanager
def _canonical_gzip_writer(path: Path) -> Iterator[csv.DictWriter]:
    created = CreatedGzipOutput.create(path)
    try:
        yield created.writer
        created.finalize()
    finally:
        created.close()


def _write_receipt(path: Path, value: Mapping[str, Any]) -> dict[str, Any]:
    flags = (
        os.O_RDWR
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    try:
        descriptor = os.open(path, flags, 0o600)
    except OSError as exc:
        raise UnionError(f"cannot create output receipt: {exc}") from exc
    with os.fdopen(descriptor, "w+b", buffering=0) as handle:
        initial = os.fstat(handle.fileno())
        if not stat.S_ISREG(initial.st_mode) or initial.st_nlink != 1:
            raise UnionError("new output receipt is not a one-link regular file")
        handle.write(canonical_json_bytes(value))
        handle.flush()
        os.fsync(handle.fileno())
        info = os.fstat(handle.fileno())
        path_info = path.stat(follow_symlinks=False)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_nlink != 1
            or not stat.S_ISREG(path_info.st_mode)
            or path_info.st_nlink != 1
            or (info.st_dev, info.st_ino) != (initial.st_dev, initial.st_ino)
            or (path_info.st_dev, path_info.st_ino) != (initial.st_dev, initial.st_ino)
        ):
            raise UnionError("output receipt identity changed during publication")
        digest, size = _hash_stream(handle)
        final_info = os.fstat(handle.fileno())
        final_path_info = path.stat(follow_symlinks=False)
        if (
            not stat.S_ISREG(final_info.st_mode)
            or final_info.st_nlink != 1
            or not stat.S_ISREG(final_path_info.st_mode)
            or final_path_info.st_nlink != 1
            or (final_info.st_dev, final_info.st_ino) != (initial.st_dev, initial.st_ino)
            or (final_path_info.st_dev, final_path_info.st_ino)
            != (initial.st_dev, initial.st_ino)
        ):
            raise UnionError("output receipt identity changed during publication")
        return {
            "path": _display_path(path),
            "bytes": size,
            "sha256": digest,
            "mode": f"{stat.S_IMODE(info.st_mode):04o}",
            "nlink": info.st_nlink,
        }


def _build(
    preregistration: PinnedFile,
    contract: Contract,
    *,
    minimum_free_bytes: int,
) -> dict[str, Any]:
    if minimum_free_bytes < DEFAULT_MINIMUM_FREE_BYTES:
        raise UnionError("minimum free-byte floor may not be weakened below 5 GiB")
    _preflight_outputs(contract)
    _require_free_bytes(
        contract.output_csv.parent,
        minimum_free_bytes + OUTPUT_RESERVE_BYTES,
        "union output preflight",
    )
    _require_free_bytes(
        contract.output_receipt.parent,
        minimum_free_bytes + RECEIPT_RESERVE_BYTES,
        "union receipt preflight",
    )
    specs = (contract.baseline, *contract.overlays)
    opened_builders: dict[str, PinnedFile] = {}
    opened_receipts: dict[str, PinnedFile] = {}
    licenses: dict[str, Mapping[str, Any]] = {}
    created_output: CreatedGzipOutput | None = None
    try:
        for spec in specs:
            opened_builders[spec.name] = _open_pinned(
                spec.builder_csv, spec.builder_pin, f"{spec.name} builder"
            )
            opened_receipts[spec.name] = _open_pinned(
                spec.merge_receipt, spec.receipt_pin, f"{spec.name} merge receipt"
            )
        for spec in specs:
            licenses[spec.name] = _validate_receipt(
                spec,
                _read_json_object(
                    opened_receipts[spec.name], f"{spec.name} merge receipt"
                ),
                baseline=contract.baseline,
            )
        overlay_files = {name: opened_builders[name] for name in OVERLAY_NAMES}
        audit_counts = _stream_pass(
            opened_builders["baseline"],
            overlay_files,
            expected_rows=contract.expected["rows"],
        )
        _assert_expected(audit_counts, contract.expected)
        _require_free_bytes(
            contract.output_csv.parent,
            minimum_free_bytes + OUTPUT_RESERVE_BYTES,
            "union output creation",
        )
        _require_free_bytes(
            contract.output_receipt.parent,
            minimum_free_bytes + RECEIPT_RESERVE_BYTES,
            "union receipt reservation",
        )
        created_output = CreatedGzipOutput.create(contract.output_csv)
        write_counts = _stream_pass(
            opened_builders["baseline"],
            overlay_files,
            expected_rows=contract.expected["rows"],
            writer=created_output.writer,
        )
        output_evidence = created_output.finalize()
        if write_counts != audit_counts:
            raise UnionError("union inputs changed between audit and write passes")
        for name, pinned in opened_builders.items():
            _recheck_pinned(pinned, f"{name} builder")
        for name, pinned in opened_receipts.items():
            _recheck_pinned(pinned, f"{name} merge receipt")
        _recheck_pinned(preregistration, "union preregistration")
        created_output.recheck()
        _require_free_bytes(
            contract.output_csv.parent,
            minimum_free_bytes,
            "union output post-write",
        )
        _require_free_bytes(
            contract.output_receipt.parent,
            minimum_free_bytes + RECEIPT_RESERVE_BYTES,
            "union receipt publication",
        )
        receipt: dict[str, Any] = {
            "schema": SCHEMA,
            "status": STATUS,
            "preregistration": dict(preregistration.evidence),
            "policy": {
                "outcome_blind_exhaustive_all_four_union": True,
                "base_nonblank_postcode_precedence": True,
                "blank_base_requires_equal_five_digit_postal_display_pair": True,
                "one_distinct_proposal_required_to_fill": True,
                "conflicting_rows_left_blank": True,
                "first_candidate_selection": False,
                "source_order_priority": False,
                "source_coordinates_used_for_selection": False,
                "truth_or_result_coordinates_read": False,
                "benchmark_outcomes_read": False,
                "base_row_order_preserved": True,
                "non_postcode_cells_preserved": True,
                "source_rows_added": False,
                "network_calls_during_build": 0,
                "gridpin_engine_calls_during_build": 0,
                "photon_engine_calls_during_build": 0,
            },
            "configuration": {
                "builder_header": list(BUILDER_HEADER),
                "closed_overlay_set": sorted(OVERLAY_NAMES),
                "expected": dict(contract.expected),
                "minimum_free_bytes": minimum_free_bytes,
            },
            "inputs": {
                "baseline": {
                    "builder_csv": dict(opened_builders["baseline"].evidence),
                    "merge_receipt": dict(opened_receipts["baseline"].evidence),
                    "receipt_schema": contract.baseline.expected_receipt_schema,
                },
                "overlays": {
                    spec.name: {
                        "builder_csv": dict(opened_builders[spec.name].evidence),
                        "merge_receipt": dict(opened_receipts[spec.name].evidence),
                        "receipt_schema": spec.expected_receipt_schema,
                    }
                    for spec in contract.overlays
                },
            },
            "licenses": {name: dict(licenses[name]) for name in ("baseline", *OVERLAY_NAMES)},
            "counts": audit_counts,
            "output": output_evidence,
        }
        _write_receipt(contract.output_receipt, receipt)
        created_output.recheck()
        _require_free_bytes(
            contract.output_csv.parent,
            minimum_free_bytes,
            "union output completion",
        )
        _require_free_bytes(
            contract.output_receipt.parent,
            minimum_free_bytes,
            "union receipt completion",
        )
        return receipt
    finally:
        if created_output is not None:
            created_output.close()
        for pinned in (*opened_builders.values(), *opened_receipts.values()):
            pinned.stream.close()


def build(
    *,
    preregistration_path: Path,
    preregistration_pin: Pin,
    output_csv: Path,
    output_receipt: Path,
    minimum_free_bytes: int = DEFAULT_MINIMUM_FREE_BYTES,
) -> dict[str, Any]:
    """Load the exact production preregistration and build its union once."""

    preregistration = _open_pinned(
        preregistration_path, preregistration_pin, "union preregistration"
    )
    try:
        contract = _load_contract(
            preregistration,
            output_csv=output_csv,
            output_receipt=output_receipt,
            required_expected=PRODUCTION_EXPECTED,
        )
        return _build(
            preregistration,
            contract,
            minimum_free_bytes=minimum_free_bytes,
        )
    finally:
        preregistration.stream.close()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--preregistration", type=Path, required=True)
    parser.add_argument("--preregistration-sha256", required=True)
    parser.add_argument("--preregistration-bytes", type=int, required=True)
    parser.add_argument("--output-csv", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument(
        "--minimum-free-bytes", type=int, default=DEFAULT_MINIMUM_FREE_BYTES
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        receipt = build(
            preregistration_path=args.preregistration,
            preregistration_pin=Pin(
                args.preregistration_sha256, args.preregistration_bytes
            ),
            output_csv=args.output_csv,
            output_receipt=args.receipt,
            minimum_free_bytes=args.minimum_free_bytes,
        )
    except UnionError as exc:
        raise SystemExit(f"DE local postcode overlay union refused: {exc}") from exc
    print(json.dumps(receipt, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
