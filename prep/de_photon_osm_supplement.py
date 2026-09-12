#!/usr/bin/env python3
"""Build a deterministic LAB-only OSM address supplement for Germany.

This tool consumes a *pinned local* Photon JSONL/Zstandard dump and the pinned,
already-sorted GridPin Germany builder CSV.  It never opens the network and it
does not invoke Photon or GridPin.  The result is another canonical builder CSV
whose OSM-derived rows make it subject to the ODbL.  It is deliberately marked
``LAB_ONLY_ODBL_NOT_SHIPPABLE`` and must not replace a distributable Germany
sheet without a separate legal/release decision.

The implementation is streaming apart from a bounded in-memory sort chunk.
Temporary runs are gzip-compressed, then merged with the existing sorted CSV.
No uncompressed copy of the 71 GB dump or of the combined corpus is created.
"""
from __future__ import annotations

import argparse
import base64
import contextlib
import csv
import dataclasses
import gzip
import hashlib
import heapq
import io
import json
import math
import os
import pathlib
import re
import shutil
import stat
import subprocess
import tempfile
import unicodedata
import uuid
from collections import defaultdict
from collections.abc import Callable, Iterable, Iterator, Mapping, Sequence
from typing import Any, BinaryIO, TextIO


LAB_STATUS = "LAB_ONLY_ODBL_NOT_SHIPPABLE"
SCHEMA = "gridpin-de-photon-osm-supplement-v1"
VALIDATION_SCHEMA = "gridpin-de-photon-jsonl-validation-v1"
ODBL_LICENSE = "Open Database License 1.0"
ODBL_LICENSE_URL = "https://opendatacommons.org/licenses/odbl/1-0/"
OSM_ATTRIBUTION = "© OpenStreetMap contributors"
OSM_ATTRIBUTION_URL = "https://www.openstreetmap.org/copyright"

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
RUN_HEADER = BUILDER_HEADER + (
    "source_priority",
    "osm_object_type",
    "osm_object_id",
    "osm_place_id",
)

DEFAULT_MIN_FREE_BYTES = 5 * 2**30
DEFAULT_CHUNK_ROWS = 300_000
MAX_CHUNK_ROWS = 1_000_000
DEFAULT_LOCALITY_DISTANCE_M = 50_000.0
MAX_JSONL_LINE_BYTES = 16 * 2**20
MAX_OVERTURE_IDENTITY_ROWS = 100_000
MAX_VALIDATION_ANOMALIES = 10_000
_SHA256 = re.compile(r"[0-9a-f]{64}")
_POSTCODE = re.compile(r"[0-9]{5}")
# Deliberately excludes every second digit and list/range separator.  GridPin's
# builder represents one integer plus one suffix; collapsing "23-25" to 23
# would invent an address and make later range handling scientifically false.
_SINGLE_HOUSE = re.compile(r"^\s*([0-9]+)\s*([^0-9,;/\\-]*)\s*$", re.UNICODE)
_PUNCTUATION = re.compile(r"[-'’`./,;()ʻʼ‘]")
_SPACE = re.compile(r"\s+")
_REP_PUNCTUATION = re.compile(r"[\s\-/\.ʻʼ‘]")


class SupplementError(RuntimeError):
    """The LAB supplement could not be produced without weakening a guard."""


@dataclasses.dataclass(frozen=True)
class Pin:
    sha256: str
    bytes: int


@dataclasses.dataclass(frozen=True)
class Config:
    photon_dump: pathlib.Path
    photon_pin: Pin
    expected_photon_jsonl_records: int
    expected_photon_uncompressed_bytes: int
    expected_photon_version: str
    expected_photon_database_version: str
    expected_photon_data_timestamp: str
    expected_overture_rows: int
    expected_osm_candidate_rows: int
    expected_rejected_non_single_house: int
    overture_csv: pathlib.Path
    overture_pin: Pin
    output_csv: pathlib.Path
    receipt: pathlib.Path
    temp_dir: pathlib.Path
    chunk_rows: int = DEFAULT_CHUNK_ROWS
    minimum_free_bytes: int = DEFAULT_MIN_FREE_BYTES
    locality_max_distance_m: float = DEFAULT_LOCALITY_DISTANCE_M
    zstd_binary: str = "zstd"
    lab_acknowledgement: str = ""


@dataclasses.dataclass(frozen=True)
class ValidationConfig:
    photon_dump: pathlib.Path
    photon_pin: Pin
    expected_photon_jsonl_records: int
    expected_photon_uncompressed_bytes: int
    expected_photon_version: str
    expected_photon_database_version: str
    expected_photon_data_timestamp: str
    receipt: pathlib.Path
    quarantine_dir: pathlib.Path
    minimum_free_bytes: int = DEFAULT_MIN_FREE_BYTES
    zstd_binary: str = "zstd"


@dataclasses.dataclass
class Locality:
    code: str
    display: str
    norm: str
    province: str
    lat_sum: float = 0.0
    lon_sum: float = 0.0
    rows: int = 0
    postcodes: set[str] = dataclasses.field(default_factory=set)

    @property
    def lat(self) -> float:
        return self.lat_sum / self.rows

    @property
    def lon(self) -> float:
        return self.lon_sum / self.rows


@dataclasses.dataclass(frozen=True)
class LocalityResolution:
    code: str
    display: str
    norm: str
    province: str
    hashed: bool


@dataclasses.dataclass(frozen=True)
class OSMIdentityCandidate:
    identity: tuple[str, str, int, str]
    row: dict[str, str]
    postcode_consensus: bool
    witness_rows: int


@dataclasses.dataclass(frozen=True)
class PinnedFile:
    path: pathlib.Path
    stream: BinaryIO
    stat_result: os.stat_result
    evidence: Mapping[str, Any]


class DiskGuard:
    """Continuous free-space floor for every filesystem used by the run."""

    def __init__(
        self,
        paths: Sequence[pathlib.Path],
        minimum_free_bytes: int,
        *,
        disk_usage: Callable[[str | os.PathLike[str]], Any] = shutil.disk_usage,
    ) -> None:
        self.minimum = minimum_free_bytes
        self._disk_usage = disk_usage
        unique: dict[int, pathlib.Path] = {}
        for path in paths:
            anchor = path if path.is_dir() else path.parent
            anchor = anchor.resolve(strict=True)
            unique[os.stat(anchor).st_dev] = anchor
        self.paths = tuple(unique.values())

    def check(self, stage: str) -> None:
        for path in self.paths:
            free = int(self._disk_usage(path).free)
            if free < self.minimum:
                raise SupplementError(
                    f"disk floor crossed at {stage}: {path} has {free} free bytes, "
                    f"requires at least {self.minimum}"
                )


def normalize_text(value: str) -> str:
    """Mirror the canonical Overture key normalization without casefolding ß."""

    decomposed = unicodedata.normalize("NFKD", value.lower().replace("đ", "d"))
    unaccented = "".join(ch for ch in decomposed if not unicodedata.combining(ch))
    return _SPACE.sub(" ", _PUNCTUATION.sub(" ", unaccented)).strip()


def normalize_rep(value: str) -> str:
    return _REP_PUNCTUATION.sub("", value.lower()).strip()


def parse_single_house(value: object) -> tuple[int, str] | None:
    if not isinstance(value, str):
        return None
    match = _SINGLE_HOUSE.fullmatch(value)
    if match is None:
        return None
    number = int(match.group(1))
    if number > 0xFFFF_FFFF:
        return None
    return number, normalize_rep(match.group(2))


def _reject_constant(value: str) -> None:
    raise ValueError(f"non-finite JSON constant {value}")


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def strict_json_loads(raw: str) -> Any:
    return json.loads(
        raw,
        parse_constant=_reject_constant,
        object_pairs_hook=_unique_object,
    )


def canonical_json_bytes(value: Mapping[str, Any]) -> bytes:
    return (
        json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        + "\n"
    ).encode("utf-8")


def _mode(info: os.stat_result) -> str:
    return f"{stat.S_IMODE(info.st_mode):04o}"


def _validate_pin(pin: Pin, label: str) -> None:
    if _SHA256.fullmatch(pin.sha256) is None:
        raise SupplementError(f"{label} SHA-256 must be 64 lowercase hex characters")
    if pin.bytes < 0:
        raise SupplementError(f"{label} byte pin must be non-negative")


def _open_pinned(path: pathlib.Path, pin: Pin, label: str) -> PinnedFile:
    _validate_pin(pin, label)
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags)
    except OSError as exc:
        raise SupplementError(f"cannot open pinned {label}: {path}: {exc}") from exc
    stream = os.fdopen(fd, "rb", closefd=True)
    try:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise SupplementError(f"pinned {label} must be a regular nlink=1 file")
        if before.st_size != pin.bytes:
            raise SupplementError(
                f"{label} size pin mismatch: got {before.st_size}, expected {pin.bytes}"
            )
        digest = hashlib.sha256()
        while block := stream.read(8 * 2**20):
            digest.update(block)
        actual = digest.hexdigest()
        if actual != pin.sha256:
            raise SupplementError(
                f"{label} SHA-256 pin mismatch: got {actual}, expected {pin.sha256}"
            )
        named = os.stat(path, follow_symlinks=False)
        if (named.st_dev, named.st_ino) != (before.st_dev, before.st_ino):
            raise SupplementError(f"pinned {label} pathname changed during verification")
        stream.seek(0)
        return PinnedFile(
            path=path,
            stream=stream,
            stat_result=before,
            evidence={
                "path": str(path),
                "bytes": before.st_size,
                "sha256": actual,
                "mode": _mode(before),
                "nlink": before.st_nlink,
            },
        )
    except BaseException:
        stream.close()
        raise


def _recheck_pinned(pinned: PinnedFile, label: str) -> None:
    held = os.fstat(pinned.stream.fileno())
    named = os.stat(pinned.path, follow_symlinks=False)
    identity = (pinned.stat_result.st_dev, pinned.stat_result.st_ino)
    if (held.st_dev, held.st_ino) != identity or (named.st_dev, named.st_ino) != identity:
        raise SupplementError(f"pinned {label} identity changed during processing")
    if (
        not stat.S_ISREG(held.st_mode)
        or held.st_size != pinned.stat_result.st_size
        or held.st_nlink != 1
    ):
        raise SupplementError(f"pinned {label} metadata changed during processing")
    position = pinned.stream.tell()
    digest = hashlib.sha256()
    pinned.stream.seek(0)
    try:
        while block := pinned.stream.read(8 * 2**20):
            digest.update(block)
    finally:
        pinned.stream.seek(position)
    if digest.hexdigest() != pinned.evidence["sha256"]:
        raise SupplementError(f"pinned {label} content changed during processing")


def _haversine_m(lat1: float, lon1: float, lat2: float, lon2: float) -> float:
    radius = 6_371_008.8
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp = p2 - p1
    dl = math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * radius * math.asin(min(1.0, math.sqrt(a)))


def _postcode(row: Mapping[str, str]) -> str:
    display = row.get("code_postal_display", "").strip()
    numeric = row.get("code_postal", "").strip()
    if _POSTCODE.fullmatch(display):
        return display
    if _POSTCODE.fullmatch(numeric):
        return numeric
    return ""


def _csv_text_reader(stream: BinaryIO, *, gzipped: bool) -> tuple[TextIO, csv.DictReader]:
    binary: BinaryIO
    if gzipped:
        binary = gzip.GzipFile(fileobj=stream, mode="rb")
    else:
        binary = stream
    text = io.TextIOWrapper(binary, encoding="utf-8", errors="strict", newline="")
    reader = csv.DictReader(text)
    return text, reader


def _validated_float(raw: str, name: str) -> float:
    try:
        value = float(raw)
    except ValueError as exc:
        raise SupplementError(f"invalid {name} in canonical Overture CSV: {raw!r}") from exc
    if not math.isfinite(value):
        raise SupplementError(f"non-finite {name} in canonical Overture CSV")
    if (name == "lat" and abs(value) > 90) or (name == "lon" and abs(value) > 180):
        raise SupplementError(f"out-of-range {name} in canonical Overture CSV")
    return value


class LocalityResolver:
    def __init__(
        self,
        localities: Iterable[Locality],
        max_distance_m: float,
        *,
        hash_fn: Callable[[bytes], str] | None = None,
    ) -> None:
        self.by_city: dict[str, list[Locality]] = defaultdict(list)
        self.used_codes: set[str] = set()
        for locality in localities:
            self.by_city[locality.norm].append(locality)
            self.used_codes.add(locality.code)
        for values in self.by_city.values():
            values.sort(key=lambda value: value.code)
        self.max_distance_m = max_distance_m
        self._hash_fn = hash_fn or (lambda raw: hashlib.sha256(raw).hexdigest())
        self._hashed_by_key: dict[str, str] = {}
        self._key_by_hashed: dict[str, str] = {}

    def _hashed(self, state: str, county: str, city: str) -> str:
        key = "\x1f".join((state, county, city))
        existing = self._hashed_by_key.get(key)
        if existing is not None:
            return existing
        digest = self._hash_fn(key.encode("utf-8"))
        if _SHA256.fullmatch(digest) is None:
            raise SupplementError("locality hash function returned a non-SHA256 digest")
        # builder.rs stores code_insee in exactly eight bytes.  `M` + seven
        # base32 characters carries 35 bits and fits that hard format gate.
        # At national scale truncation collisions are possible, therefore both
        # hash↔hash and hash↔base collisions are terminal rather than resolved
        # by input-order-dependent probing.
        code = "M" + base64.b32encode(bytes.fromhex(digest)).decode("ascii")[:7]
        other = self._key_by_hashed.get(code)
        if other is not None and other != key:
            raise SupplementError(f"hashed locality collision for {code}: {other!r} vs {key!r}")
        if code in self.used_codes:
            raise SupplementError(f"hashed locality {code} collides with an Overture code")
        self._hashed_by_key[key] = code
        self._key_by_hashed[code] = key
        self.used_codes.add(code)
        return code

    def resolve(
        self,
        *,
        state: str,
        county: str,
        city: str,
        postcode: str,
        lat: float,
        lon: float,
    ) -> LocalityResolution:
        city_norm = normalize_text(city)
        candidates = list(self.by_city.get(city_norm, ()))
        postcode_matches = [value for value in candidates if postcode in value.postcodes]
        pool = postcode_matches or candidates
        if pool:
            chosen = min(
                pool,
                key=lambda value: (
                    _haversine_m(lat, lon, value.lat, value.lon),
                    value.code,
                ),
            )
            distance = _haversine_m(lat, lon, chosen.lat, chosen.lon)
            if distance <= self.max_distance_m:
                # Keep the frozen Overture commune spelling and normalization;
                # a Photon spelling must not mutate an existing locality code.
                return LocalityResolution(
                    code=chosen.code,
                    display=chosen.display,
                    norm=chosen.norm,
                    province=chosen.province,
                    hashed=False,
                )
        return LocalityResolution(
            code=self._hashed(normalize_text(state), normalize_text(county), city_norm),
            display=city.strip(),
            norm=city_norm,
            province=normalize_text(state),
            hashed=True,
        )

    @property
    def hashed_localities(self) -> int:
        return len(self._hashed_by_key)


def load_localities(
    pinned: PinnedFile,
) -> tuple[tuple[Locality, ...], dict[str, int]]:
    pinned.stream.seek(0)
    text, reader = _csv_text_reader(
        pinned.stream, gzipped=pinned.path.suffix.lower() == ".gz"
    )
    groups: dict[str, Locality] = {}
    counts = {"overture_rows_scanned_for_localities": 0}
    try:
        if tuple(reader.fieldnames or ()) != BUILDER_HEADER:
            raise SupplementError(
                "Overture builder CSV header drift: "
                f"got {reader.fieldnames!r}, expected {list(BUILDER_HEADER)!r}"
            )
        for row in reader:
            counts["overture_rows_scanned_for_localities"] += 1
            code = row["code_insee"].strip()
            norm = row["nom_commune_norm"].strip()
            if not code or not norm or len(code.encode("utf-8")) > 8:
                raise SupplementError("invalid locality identity in canonical Overture CSV")
            lat = _validated_float(row["lat"], "lat")
            lon = _validated_float(row["lon"], "lon")
            locality = groups.get(code)
            province = row["provincia_norm"].strip()
            if locality is None:
                locality = Locality(
                    code=code,
                    display=row["nom_commune"],
                    norm=norm,
                    province=province,
                )
                groups[code] = locality
            elif locality.norm != norm:
                raise SupplementError(f"Overture locality code {code} maps to two names")
            elif province and locality.province and locality.province != province:
                raise SupplementError(f"Overture locality code {code} maps to two provinces")
            elif province:
                locality.province = province
            locality.lat_sum += lat
            locality.lon_sum += lon
            locality.rows += 1
            postcode = _postcode(row)
            if postcode:
                locality.postcodes.add(postcode)
    finally:
        # GzipFile does not close the caller-owned fileobj, so closing this
        # wrapper is both leak-free and compatible with the later pinned pass.
        text.close()
    return tuple(groups.values()), counts


def _feature_priority(feature: Mapping[str, Any]) -> int:
    key = feature.get("osm_key")
    value = feature.get("osm_value")
    object_type = feature.get("object_type")
    if key == "place" and value == "house" and object_type == "N":
        return 0
    if key == "building" and object_type in {"W", "R"}:
        return 1
    if object_type == "N":
        return 2
    return 3


def _feature_to_run_row(
    feature: Mapping[str, Any],
    resolver: LocalityResolver,
    counts: defaultdict[str, int],
) -> dict[str, str] | None:
    if feature.get("country_code") != "de":
        counts["rejected_not_de"] += 1
        return None
    housenumber = feature.get("housenumber")
    if not isinstance(housenumber, str) or not housenumber.strip():
        counts["rejected_missing_housenumber"] += 1
        return None
    counts["de_housenumber_features"] += 1
    address = feature.get("address")
    if not isinstance(address, dict):
        counts["rejected_address_schema"] += 1
        return None
    street = address.get("street")
    city = address.get("city")
    postcode = feature.get("postcode")
    centroid = feature.get("centroid")
    if not isinstance(street, str) or not normalize_text(street):
        counts["rejected_missing_street"] += 1
        return None
    if not isinstance(city, str) or not normalize_text(city):
        counts["rejected_missing_city"] += 1
        return None
    if not isinstance(postcode, str) or _POSTCODE.fullmatch(postcode.strip()) is None:
        counts["rejected_postcode"] += 1
        return None
    if (
        not isinstance(centroid, list)
        or len(centroid) < 2
        or not all(
            isinstance(value, (int, float)) and not isinstance(value, bool)
            for value in centroid[:2]
        )
    ):
        counts["rejected_centroid"] += 1
        return None
    lon, lat = float(centroid[0]), float(centroid[1])
    if not math.isfinite(lat) or not math.isfinite(lon) or abs(lat) > 90 or abs(lon) > 180:
        counts["rejected_centroid"] += 1
        return None
    counts["source_ready_housenumber_features"] += 1
    parsed = parse_single_house(housenumber)
    if parsed is None:
        counts["rejected_non_single_house"] += 1
        return None
    number, suffix = parsed
    postcode = postcode.strip()
    state = address.get("state") if isinstance(address.get("state"), str) else ""
    county = address.get("county") if isinstance(address.get("county"), str) else ""
    locality = resolver.resolve(
        state=state,
        county=county,
        city=city,
        postcode=postcode,
        lat=lat,
        lon=lon,
    )
    counts[
        "locality_hashed_rows" if locality.hashed else "locality_overture_rows"
    ] += 1
    object_type = feature.get("object_type")
    object_id = feature.get("object_id")
    place_id = feature.get("place_id")
    if object_type not in {"N", "W", "R"}:
        raise SupplementError(f"unexpected OSM object_type {object_type!r}")
    if not isinstance(object_id, int) or isinstance(object_id, bool) or object_id < 0:
        raise SupplementError(f"invalid OSM object_id {object_id!r}")
    if not isinstance(place_id, str) or not place_id.isdigit():
        raise SupplementError(f"invalid Photon place_id {place_id!r}")
    return {
        "nom_voie_norm": normalize_text(street),
        "code_insee": locality.code,
        "nom_commune_norm": locality.norm,
        "code_postal": postcode,
        "code_postal_display": postcode,
        "numero": str(number),
        "rep": suffix,
        "lon": format(lon, ".15g"),
        "lat": format(lat, ".15g"),
        "nom_voie": street.strip(),
        "nom_commune": locality.display,
        "provincia_norm": locality.province,
        "source_priority": str(_feature_priority(feature)),
        "osm_object_type": object_type,
        "osm_object_id": str(object_id),
        "osm_place_id": place_id,
    }


def _identity(row: Mapping[str, str]) -> tuple[str, str, int, str]:
    try:
        number = int(row["numero"])
    except (KeyError, TypeError, ValueError) as exc:
        raise SupplementError("invalid house number in builder/run CSV") from exc
    if number < 0 or number > 0xFFFF_FFFF:
        raise SupplementError("out-of-range house number in builder/run CSV")
    return (
        row["nom_voie_norm"],
        row["code_insee"],
        number,
        row["rep"],
    )


def _run_sort_key(row: Mapping[str, str]) -> tuple[Any, ...]:
    return _identity(row) + (
        int(row["source_priority"]),
        int(row["osm_object_id"]),
        row["osm_object_type"],
        int(row["osm_place_id"]),
        row["code_postal_display"],
        float(row["lon"]),
        float(row["lat"]),
    )


def _overture_sort_key(row: Mapping[str, str]) -> tuple[Any, ...]:
    return _identity(row) + (float(row["lon"]), float(row["lat"]))


@contextlib.contextmanager
def _canonical_gzip_writer(path: pathlib.Path, header: Sequence[str]) -> Iterator[csv.DictWriter]:
    raw = open(path, "xb", buffering=0)
    os.chmod(path, 0o600)
    try:
        gz = gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=6, mtime=0)
        text = io.TextIOWrapper(gz, encoding="utf-8", errors="strict", newline="")
        detached = False
        writer = csv.DictWriter(
            text,
            fieldnames=header,
            extrasaction="raise",
            lineterminator="\n",
        )
        writer.writeheader()
        try:
            yield writer
            text.flush()
            text.detach()
            detached = True
            gz.close()
            os.fsync(raw.fileno())
        finally:
            if not detached:
                with contextlib.suppress(Exception):
                    text.close()
                with contextlib.suppress(Exception):
                    gz.close()
    finally:
        raw.close()


def _write_run(
    rows: list[dict[str, str]], run_dir: pathlib.Path, index: int
) -> pathlib.Path:
    rows.sort(key=_run_sort_key)
    path = run_dir / f"run-{index:06d}.csv.gz"
    with _canonical_gzip_writer(path, RUN_HEADER) as writer:
        writer.writerows(rows)
    return path


def _iter_run(path: pathlib.Path) -> Iterator[dict[str, str]]:
    with gzip.open(path, "rt", encoding="utf-8", errors="strict", newline="") as stream:
        reader = csv.DictReader(stream)
        if tuple(reader.fieldnames or ()) != RUN_HEADER:
            raise SupplementError(f"temporary run header drift in {path}")
        previous: tuple[Any, ...] | None = None
        for row in reader:
            key = _run_sort_key(row)
            if previous is not None and key < previous:
                raise SupplementError(f"temporary run is not sorted: {path}")
            previous = key
            yield row


def _merged_runs(paths: Sequence[pathlib.Path]) -> Iterator[dict[str, str]]:
    iterators = [iter(_iter_run(path)) for path in paths]
    heap: list[tuple[tuple[Any, ...], int, dict[str, str]]] = []
    for index, iterator in enumerate(iterators):
        try:
            row = next(iterator)
        except StopIteration:
            continue
        heapq.heappush(heap, (_run_sort_key(row), index, row))
    while heap:
        _, index, row = heapq.heappop(heap)
        yield row
        try:
            following = next(iterators[index])
        except StopIteration:
            continue
        heapq.heappush(heap, (_run_sort_key(following), index, following))


def _consensus_rows(
    rows: Iterable[dict[str, str]], counts: defaultdict[str, int]
) -> Iterator[OSMIdentityCandidate]:
    current: tuple[str, str, int, str] | None = None
    chosen: dict[str, str] | None = None
    postcode = ""
    group_rows = 0
    conflict = False

    def finish() -> OSMIdentityCandidate | None:
        if current is None or chosen is None:
            return None
        if conflict:
            counts["osm_conflict_identities"] += 1
            counts["osm_conflict_rows"] += group_rows
        else:
            counts["osm_consensus_identities"] += 1
            counts["osm_duplicate_rows_removed"] += group_rows - 1
        return OSMIdentityCandidate(
            identity=current,
            row=chosen,
            postcode_consensus=not conflict,
            witness_rows=group_rows,
        )

    for row in rows:
        identity = _identity(row)
        if current is not None and identity != current:
            completed = finish()
            if completed is not None:
                yield completed
            chosen = None
            postcode = ""
            group_rows = 0
            conflict = False
        current = identity
        group_rows += 1
        if chosen is None:
            # Input is sorted by _run_sort_key, so this is the deterministic
            # priority/OSM-id/geometry winner without retaining the full group.
            chosen = row
            postcode = row["code_postal_display"]
        elif row["code_postal_display"] != postcode:
            conflict = True
    completed = finish()
    if completed is not None:
        yield completed


def _iter_overture_groups(pinned: PinnedFile) -> Iterator[tuple[tuple[str, str, int, str], list[dict[str, str]]]]:
    pinned.stream.seek(0)
    text, reader = _csv_text_reader(
        pinned.stream, gzipped=pinned.path.suffix.lower() == ".gz"
    )
    try:
        if tuple(reader.fieldnames or ()) != BUILDER_HEADER:
            raise SupplementError("Overture builder CSV header changed between passes")
        current: tuple[str, str, int, str] | None = None
        group: list[dict[str, str]] = []
        previous_sort: tuple[Any, ...] | None = None
        for row in reader:
            sort_key = _overture_sort_key(row)
            if previous_sort is not None and sort_key < previous_sort:
                raise SupplementError("pinned Overture builder CSV is not canonically sorted")
            previous_sort = sort_key
            identity = _identity(row)
            if current is not None and identity != current:
                yield current, group
                group = []
            current = identity
            group.append(row)
            if len(group) > MAX_OVERTURE_IDENTITY_ROWS:
                raise SupplementError(
                    "Overture identity group exceeds bounded merge limit "
                    f"{MAX_OVERTURE_IDENTITY_ROWS}"
                )
        if current is not None:
            yield current, group
    finally:
        text.close()


def _next_or_none(iterator: Iterator[Any]) -> Any | None:
    try:
        return next(iterator)
    except StopIteration:
        return None


def _builder_projection(row: Mapping[str, str]) -> dict[str, str]:
    return {name: row[name] for name in BUILDER_HEADER}


def _merge_output(
    pinned: PinnedFile,
    osm_rows: Iterator[OSMIdentityCandidate],
    writer: csv.DictWriter,
    counts: defaultdict[str, int],
) -> None:
    overture = iter(_iter_overture_groups(pinned))
    osm_groups = iter(_unique_osm_rows(osm_rows))
    left = _next_or_none(overture)
    right = _next_or_none(osm_groups)
    while left is not None or right is not None:
        if right is None or (left is not None and left[0] < right.identity):
            for row in left[1]:
                writer.writerow(row)
                counts["output_rows"] += 1
                counts["overture_rows_retained"] += 1
            left = _next_or_none(overture)
            continue
        if left is None or right.identity < left[0]:
            # OSM-only identities always get one deterministic priority winner.
            writer.writerow(_builder_projection(right.row))
            counts["output_rows"] += 1
            counts["osm_only_rows_added"] += 1
            if not right.postcode_consensus:
                counts["osm_only_conflict_winner_rows_added"] += 1
            right = _next_or_none(osm_groups)
            continue

        overture_rows = left[1]
        osm = right.row
        if not right.postcode_consensus:
            for row in overture_rows:
                writer.writerow(row)
                counts["output_rows"] += 1
                counts["overture_rows_retained"] += 1
            counts["osm_conflict_identities_with_overture"] += 1
            counts["osm_rows_dropped_by_overture_precedence"] += 1
            left = _next_or_none(overture)
            right = _next_or_none(osm_groups)
            continue
        osm_postcode = osm["code_postal_display"]
        known = {_postcode(row) for row in overture_rows} - {""}
        if not known or known == {osm_postcode}:
            for row in overture_rows:
                projected = dict(row)
                if not _postcode(projected):
                    projected["code_postal"] = osm_postcode
                    projected["code_postal_display"] = osm_postcode
                    counts["overture_blank_postcode_rows_filled"] += 1
                writer.writerow(projected)
                counts["output_rows"] += 1
                counts["overture_rows_retained"] += 1
            counts["osm_rows_consumed_by_overture_identity"] += 1
        else:
            for row in overture_rows:
                writer.writerow(row)
                counts["output_rows"] += 1
                counts["overture_rows_retained"] += 1
            counts["overture_nonblank_postcode_conflicts"] += 1
            counts["osm_rows_dropped_by_overture_precedence"] += 1
        left = _next_or_none(overture)
        right = _next_or_none(osm_groups)


def _unique_osm_rows(
    rows: Iterable[OSMIdentityCandidate],
) -> Iterator[OSMIdentityCandidate]:
    previous: tuple[str, str, int, str] | None = None
    for candidate in rows:
        identity = candidate.identity
        if identity != _identity(candidate.row):
            raise SupplementError("OSM consensus identity does not match its chosen row")
        if previous is not None and identity <= previous:
            raise SupplementError("OSM consensus output is not strictly identity-sorted")
        previous = identity
        yield candidate


def _iter_photon_features(
    pinned: PinnedFile,
    zstd_binary: str,
    counts: defaultdict[str, int],
    expected_metadata: Mapping[str, str],
) -> tuple[Iterator[Mapping[str, Any]], dict[str, Any]]:
    metadata: dict[str, Any] = {}

    def generate() -> Iterator[Mapping[str, Any]]:
        pinned.stream.seek(0)
        try:
            process = subprocess.Popen(
                [zstd_binary, "-dc", "--single-thread"],
                stdin=pinned.stream,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
        except OSError as exc:
            raise SupplementError(f"cannot start local zstd decoder: {exc}") from exc
        assert process.stdout is not None
        assert process.stderr is not None
        header_seen = False
        failure: BaseException | None = None
        try:
            line_number = 0
            while True:
                raw = process.stdout.readline(MAX_JSONL_LINE_BYTES + 1)
                if not raw:
                    break
                line_number += 1
                counts["jsonl_lines"] += 1
                counts["jsonl_uncompressed_bytes"] += len(raw)
                if len(raw) > MAX_JSONL_LINE_BYTES:
                    raise SupplementError(
                        f"Photon JSONL line {line_number} exceeds "
                        f"{MAX_JSONL_LINE_BYTES} bytes"
                    )
                if not raw.endswith(b"\n"):
                    raise SupplementError(
                        f"Photon JSONL line {line_number} is not LF-terminated"
                    )
                try:
                    text = raw.decode("utf-8", errors="strict")
                    value = strict_json_loads(text)
                except (UnicodeDecodeError, ValueError, json.JSONDecodeError) as exc:
                    raise SupplementError(
                        f"invalid Photon JSONL at line {line_number}: {exc}"
                    ) from exc
                if not isinstance(value, dict) or not isinstance(value.get("type"), str):
                    raise SupplementError(f"invalid Photon record schema at line {line_number}")
                kind = value["type"]
                if kind == "NominatimDumpFile":
                    if header_seen or line_number != 1:
                        raise SupplementError("Photon dump metadata header is not unique and first")
                    content = value.get("content")
                    if (
                        not isinstance(content, dict)
                        or content.get("generator") != "photon"
                        or not all(
                            isinstance(content.get(name), str) and content[name]
                            for name in ("version", "database_version", "data_timestamp")
                        )
                        or not isinstance(content.get("features"), dict)
                        or content["features"].get("sorted_by_country") is not True
                        or not isinstance(
                            content["features"].get("has_addresslines"), bool
                        )
                    ):
                        raise SupplementError("Photon dump metadata is incompatible")
                    for name, expected in expected_metadata.items():
                        if content.get(name) != expected:
                            raise SupplementError(
                                f"Photon metadata pin mismatch for {name}: "
                                f"got {content.get(name)!r}, expected {expected!r}"
                            )
                    metadata.update(content)
                    header_seen = True
                    continue
                if not header_seen:
                    raise SupplementError("Photon Place data appeared before its metadata header")
                if kind == "CountryInfo":
                    counts["country_info_records"] += 1
                    continue
                if kind != "Place":
                    raise SupplementError(f"unexpected Photon record type {kind!r}")
                content = value.get("content")
                if not isinstance(content, list):
                    raise SupplementError(f"Photon Place.content is not a list at line {line_number}")
                for feature in content:
                    counts["place_features"] += 1
                    if not isinstance(feature, dict):
                        raise SupplementError(
                            f"Photon Place feature is not an object at line {line_number}"
                        )
                    yield feature
        except BaseException as exc:
            failure = exc
            with contextlib.suppress(ProcessLookupError):
                process.kill()
            raise
        finally:
            process.stdout.close()
            stderr = process.stderr.read(64 * 1024)
            process.stderr.close()
            returncode = process.wait()
            if failure is None and returncode != 0:
                message = stderr.decode("utf-8", errors="replace").strip()
                raise SupplementError(
                    f"zstd decoder failed with exit {returncode}: {message[:1000]}"
                )
        if not header_seen:
            raise SupplementError("Photon dump has no metadata header")

    return generate(), metadata


def _validation_anomaly(
    *,
    ordinal: int,
    digest: str,
    size: int,
    error: str,
    contains_housenumber: bool,
) -> dict[str, Any]:
    return {
        "ordinal": ordinal,
        "sha256": digest,
        "bytes": size,
        "error": error[:2_000],
        "contains_housenumber": contains_housenumber,
    }


def _write_all(fd: int, payload: bytes) -> None:
    view = memoryview(payload)
    while view:
        written = os.write(fd, view)
        if written <= 0:
            raise SupplementError("short write while retaining anomalous JSONL record")
        view = view[written:]


def _new_quarantine_temp(
    directory: pathlib.Path, ordinal: int
) -> tuple[int, pathlib.Path]:
    path = directory / f".record-{ordinal:09d}-{uuid.uuid4().hex}.partial"
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    try:
        fd = os.open(path, flags, 0o600)
    except OSError as exc:
        raise SupplementError(f"cannot create quarantine record {path}: {exc}") from exc
    os.fchmod(fd, 0o600)
    return fd, path


def _finish_quarantine_record(
    temporary: pathlib.Path,
    directory: pathlib.Path,
    ordinal: int,
    digest: str,
) -> dict[str, Any]:
    destination = directory / f"record-{ordinal:09d}-{digest}.jsonl"
    _commit_no_replace(temporary, destination)
    return _file_evidence(destination)


def validate_photon_dump(
    config: ValidationConfig,
    *,
    disk_usage: Callable[[str | os.PathLike[str]], Any] = shutil.disk_usage,
) -> dict[str, Any]:
    """Strictly validate every pinned JSONL record and write anreceipt.

    Parse anomalies are collected, never accepted or silently skipped.  The
    resulting receipt is committed even when anomalies exist, and callers must
    treat its non-PASS status as terminal failure for supplement extraction.
    """

    _validate_pin(config.photon_pin, "Photon dump")
    if config.expected_photon_jsonl_records < 1:
        raise SupplementError("expected_photon_jsonl_records must be positive")
    if config.expected_photon_uncompressed_bytes < 1:
        raise SupplementError("expected_photon_uncompressed_bytes must be positive")
    expected_metadata = {
        "version": config.expected_photon_version,
        "database_version": config.expected_photon_database_version,
        "data_timestamp": config.expected_photon_data_timestamp,
    }
    for name, value in expected_metadata.items():
        if not value or len(value.encode("utf-8")) > 256:
            raise SupplementError(f"expected Photon {name} must be bounded and non-empty")
    if config.minimum_free_bytes < 0:
        raise SupplementError("minimum_free_bytes must be non-negative")
    if config.receipt.exists() or config.receipt.is_symlink():
        raise SupplementError(f"refusing to overwrite existing output: {config.receipt}")
    if not config.receipt.parent.is_dir() or config.receipt.parent.is_symlink():
        raise SupplementError("validation receipt parent must be an existing real directory")
    if not config.quarantine_dir.is_dir() or config.quarantine_dir.is_symlink():
        raise SupplementError("quarantine_dir must be an existing real directory")
    quarantine_info = os.stat(config.quarantine_dir, follow_symlinks=False)
    if stat.S_IMODE(quarantine_info.st_mode) != 0o700:
        raise SupplementError("quarantine_dir must have exact mode 0700")
    if any(config.quarantine_dir.iterdir()):
        raise SupplementError("quarantine_dir must be empty before validation")
    if config.receipt.parent.resolve() == config.quarantine_dir.resolve():
        raise SupplementError("validation receipt must be outside quarantine_dir")
    if config.receipt.resolve(strict=False) == config.photon_dump.resolve(strict=True):
        raise SupplementError("validation receipt must differ from the Photon input")

    guard = DiskGuard(
        (config.receipt.parent, config.quarantine_dir),
        config.minimum_free_bytes,
        disk_usage=disk_usage,
    )
    guard.check("validation-preflight")
    pinned = _open_pinned(config.photon_dump, config.photon_pin, "Photon dump")
    receipt_temp: pathlib.Path | None = None
    try:
        pinned.stream.seek(0)
        try:
            process = subprocess.Popen(
                [config.zstd_binary, "-dc", "--single-thread"],
                stdin=pinned.stream,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=True,
            )
        except OSError as exc:
            raise SupplementError(f"cannot start local zstd decoder: {exc}") from exc
        assert process.stdout is not None
        assert process.stderr is not None
        anomalies: list[dict[str, Any]] = []
        quarantine_records: list[dict[str, Any]] = []
        anomaly_count = 0
        lines = 0
        uncompressed_bytes = 0
        header_seen = False
        needle = b'"housenumber"'
        try:
            while True:
                raw = process.stdout.readline(MAX_JSONL_LINE_BYTES + 1)
                if not raw:
                    break
                lines += 1
                digest = hashlib.sha256()
                digest.update(raw)
                size = len(raw)
                contains_housenumber = needle in raw
                oversized = len(raw) > MAX_JSONL_LINE_BYTES
                tail = raw[-(len(needle) - 1) :]
                quarantine_fd = -1
                quarantine_temp: pathlib.Path | None = None
                try:
                    if oversized:
                        guard.check("validation-before-oversized-quarantine")
                        quarantine_fd, quarantine_temp = _new_quarantine_temp(
                            config.quarantine_dir, lines
                        )
                        _write_all(quarantine_fd, raw)
                    while oversized and not raw.endswith(b"\n"):
                        raw = process.stdout.readline(MAX_JSONL_LINE_BYTES + 1)
                        if not raw:
                            break
                        digest.update(raw)
                        size += len(raw)
                        contains_housenumber = (
                            contains_housenumber or needle in tail + raw
                        )
                        tail = raw[-(len(needle) - 1) :]
                        _write_all(quarantine_fd, raw)
                    uncompressed_bytes += size

                    error = ""
                    value: Any = None
                    if oversized:
                        error = f"JSONL line exceeds {MAX_JSONL_LINE_BYTES} bytes"
                    elif not raw.endswith(b"\n"):
                        error = "JSONL line is not LF-terminated"
                    else:
                        try:
                            text = raw.decode("utf-8", errors="strict")
                            value = strict_json_loads(text)
                        except (
                            UnicodeDecodeError,
                            ValueError,
                            json.JSONDecodeError,
                        ) as exc:
                            error = f"{type(exc).__name__}: {exc}"

                    if not error:
                        if not isinstance(value, dict) or not isinstance(
                            value.get("type"), str
                        ):
                            error = "record is not an object with a string type"
                        elif lines == 1:
                            content = value.get("content")
                            if value["type"] != "NominatimDumpFile" or not isinstance(
                                content, dict
                            ):
                                error = "first record is not a NominatimDumpFile header"
                            elif (
                                content.get("generator") != "photon"
                                or not isinstance(content.get("features"), dict)
                                or content["features"].get("sorted_by_country") is not True
                                or not isinstance(
                                    content["features"].get("has_addresslines"), bool
                                )
                            ):
                                error = "Photon metadata header schema is incompatible"
                            else:
                                for name, expected in expected_metadata.items():
                                    if content.get(name) != expected:
                                        error = (
                                            f"metadata pin mismatch for {name}: "
                                            f"got {content.get(name)!r}, "
                                            f"expected {expected!r}"
                                        )
                                        break
                                header_seen = not error
                        elif value["type"] not in {"CountryInfo", "Place"}:
                            error = f"unexpected Photon record type {value['type']!r}"
                        elif value["type"] == "Place" and not isinstance(
                            value.get("content"), list
                        ):
                            error = "Photon Place.content is not a list"

                    if error:
                        guard.check("validation-before-anomaly-quarantine")
                        digest_hex = digest.hexdigest()
                        if quarantine_temp is None:
                            quarantine_fd, quarantine_temp = _new_quarantine_temp(
                                config.quarantine_dir, lines
                            )
                            _write_all(quarantine_fd, raw)
                        os.fsync(quarantine_fd)
                        os.close(quarantine_fd)
                        quarantine_fd = -1
                        evidence = _finish_quarantine_record(
                            quarantine_temp,
                            config.quarantine_dir,
                            lines,
                            digest_hex,
                        )
                        quarantine_temp = None
                        anomaly_count += 1
                        if len(anomalies) < MAX_VALIDATION_ANOMALIES:
                            item = _validation_anomaly(
                                ordinal=lines,
                                digest=digest_hex,
                                size=size,
                                error=error,
                                contains_housenumber=contains_housenumber,
                            )
                            item["quarantine"] = evidence
                            anomalies.append(item)
                            quarantine_records.append(evidence)
                finally:
                    if quarantine_fd >= 0:
                        os.close(quarantine_fd)
                    if quarantine_temp is not None:
                        with contextlib.suppress(FileNotFoundError):
                            quarantine_temp.unlink()
        except BaseException:
            with contextlib.suppress(ProcessLookupError):
                process.kill()
            raise
        finally:
            process.stdout.close()
            stderr = process.stderr.read(64 * 1024)
            process.stderr.close()
            returncode = process.wait()
        if returncode != 0:
            message = stderr.decode("utf-8", errors="replace").strip()
            raise SupplementError(
                f"zstd decoder failed with exit {returncode}: {message[:1000]}"
            )
        if lines != config.expected_photon_jsonl_records:
            raise SupplementError(
                f"Photon JSONL record-count pin mismatch: got {lines}, expected "
                f"{config.expected_photon_jsonl_records}"
            )
        if uncompressed_bytes != config.expected_photon_uncompressed_bytes:
            raise SupplementError(
                "Photon uncompressed-byte pin mismatch: "
                f"got {uncompressed_bytes}, expected "
                f"{config.expected_photon_uncompressed_bytes}"
            )
        if not header_seen and not any(item["ordinal"] == 1 for item in anomalies):
            raise SupplementError("Photon dump has no validated metadata header")
        _recheck_pinned(pinned, "Photon dump")
        guard.check("validation-before-receipt")
        status = (
            "PASS_STRICT_JSON_ALL_RECORDS"
            if anomaly_count == 0
            else "BLOCKED_STRICT_JSON_ANOMALIES"
        )
        receipt_value: dict[str, Any] = {
            "schema": VALIDATION_SCHEMA,
            "status": status,
            "input": dict(pinned.evidence),
            "expected": {
                "jsonl_records": config.expected_photon_jsonl_records,
                "uncompressed_bytes": config.expected_photon_uncompressed_bytes,
                "metadata": expected_metadata,
            },
            "observed": {
                "jsonl_records": lines,
                "uncompressed_bytes": uncompressed_bytes,
                "anomaly_count": anomaly_count,
                "reported_anomalies": len(anomalies),
                "quarantined_records": anomaly_count,
                "anomaly_report_limit": MAX_VALIDATION_ANOMALIES,
            },
            "anomalies": anomalies,
            "quarantine": {
                "directory": str(config.quarantine_dir),
                "mode": "0700",
                "records": quarantine_records,
                "full_raw_lf_records": True,
                "overwrite_policy": "O_EXCL_NO_REPLACE",
            },
            "policy": {
                "strict_json": True,
                "anomalies_accepted_or_skipped": False,
                "network_calls": 0,
                "engine_calls": 0,
                "supplement_created": False,
            },
        }
        payload = canonical_json_bytes(receipt_value)
        fd, name = tempfile.mkstemp(
            prefix=f".{config.receipt.name}.",
            suffix=".partial",
            dir=config.receipt.parent,
        )
        receipt_temp = pathlib.Path(name)
        owned_fd = fd
        try:
            os.fchmod(owned_fd, 0o600)
            stream = os.fdopen(owned_fd, "wb", closefd=True)
            owned_fd = -1
            with stream:
                stream.write(payload)
                stream.flush()
                os.fsync(stream.fileno())
        finally:
            if owned_fd >= 0:
                os.close(owned_fd)
        _commit_no_replace(receipt_temp, config.receipt)
        receipt_temp = None
        return receipt_value
    finally:
        pinned.stream.close()
        if receipt_temp is not None:
            with contextlib.suppress(FileNotFoundError):
                receipt_temp.unlink()


def _file_evidence(path: pathlib.Path) -> dict[str, Any]:
    info = os.stat(path, follow_symlinks=False)
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        while block := stream.read(8 * 2**20):
            digest.update(block)
    return {
        "path": str(path),
        "bytes": info.st_size,
        "sha256": digest.hexdigest(),
        "mode": _mode(info),
        "nlink": info.st_nlink,
    }


def _fsync_dir(path: pathlib.Path) -> None:
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _commit_no_replace(temporary: pathlib.Path, destination: pathlib.Path) -> None:
    try:
        os.link(temporary, destination, follow_symlinks=False)
    except FileExistsError as exc:
        raise SupplementError(f"refusing to overwrite existing output: {destination}") from exc
    _fsync_dir(destination.parent)
    temporary.unlink()
    _fsync_dir(destination.parent)


def _commit_pair_no_replace(
    pairs: Sequence[tuple[pathlib.Path, pathlib.Path]],
) -> None:
    """Commit a small artifact set without leaving a partial pair on collision."""

    linked: list[tuple[pathlib.Path, pathlib.Path]] = []
    try:
        for temporary, destination in pairs:
            try:
                os.link(temporary, destination, follow_symlinks=False)
            except FileExistsError as exc:
                raise SupplementError(
                    f"refusing to overwrite existing output: {destination}"
                ) from exc
            linked.append((temporary, destination))
            _fsync_dir(destination.parent)
    except BaseException:
        for temporary, destination in reversed(linked):
            try:
                temporary_info = os.stat(temporary, follow_symlinks=False)
                destination_info = os.stat(destination, follow_symlinks=False)
                if (temporary_info.st_dev, temporary_info.st_ino) == (
                    destination_info.st_dev,
                    destination_info.st_ino,
                ):
                    destination.unlink()
                    _fsync_dir(destination.parent)
            except FileNotFoundError:
                pass
        raise
    for temporary, _ in pairs:
        temporary.unlink()
    for directory in {destination.parent for _, destination in pairs}:
        _fsync_dir(directory)


def _validate_config(config: Config) -> None:
    if config.lab_acknowledgement != LAB_STATUS:
        raise SupplementError(
            f"explicit acknowledgement {LAB_STATUS!r} is required; this output is ODbL LAB-only"
        )
    _validate_pin(config.photon_pin, "Photon dump")
    _validate_pin(config.overture_pin, "Overture CSV")
    if config.expected_photon_jsonl_records < 1:
        raise SupplementError("expected_photon_jsonl_records must be positive")
    if config.expected_photon_uncompressed_bytes < 1:
        raise SupplementError("expected_photon_uncompressed_bytes must be positive")
    for name, value in (
        ("expected_overture_rows", config.expected_overture_rows),
        ("expected_osm_candidate_rows", config.expected_osm_candidate_rows),
        (
            "expected_rejected_non_single_house",
            config.expected_rejected_non_single_house,
        ),
    ):
        if value < 0:
            raise SupplementError(f"{name} must be non-negative")
    for name, value in (
        ("expected_photon_version", config.expected_photon_version),
        ("expected_photon_database_version", config.expected_photon_database_version),
        ("expected_photon_data_timestamp", config.expected_photon_data_timestamp),
    ):
        if not value or len(value.encode("utf-8")) > 256:
            raise SupplementError(f"{name} must be a non-empty bounded UTF-8 string")
    if config.chunk_rows < 1 or config.chunk_rows > MAX_CHUNK_ROWS:
        raise SupplementError(f"chunk_rows must be in 1..={MAX_CHUNK_ROWS}")
    if config.minimum_free_bytes < 0:
        raise SupplementError("minimum_free_bytes must be non-negative")
    if not math.isfinite(config.locality_max_distance_m) or config.locality_max_distance_m <= 0:
        raise SupplementError("locality_max_distance_m must be positive and finite")
    for path in (config.output_csv, config.receipt):
        if path.exists() or path.is_symlink():
            raise SupplementError(f"refusing to overwrite existing output: {path}")
        if not path.parent.is_dir() or path.parent.is_symlink():
            raise SupplementError(f"output parent must be an existing real directory: {path.parent}")
    if not config.temp_dir.is_dir() or config.temp_dir.is_symlink():
        raise SupplementError("temp_dir must be an existing real directory")
    resolved = {
        config.photon_dump.resolve(strict=True),
        config.overture_csv.resolve(strict=True),
        config.output_csv.resolve(strict=False),
        config.receipt.resolve(strict=False),
    }
    if len(resolved) != 4:
        raise SupplementError("input and output paths must all be distinct")


def build_supplement(
    config: Config,
    *,
    disk_usage: Callable[[str | os.PathLike[str]], Any] = shutil.disk_usage,
) -> dict[str, Any]:
    """Build and commit the LAB CSV plus its canonical receipt."""

    _validate_config(config)
    guard = DiskGuard(
        (config.temp_dir, config.output_csv.parent, config.receipt.parent),
        config.minimum_free_bytes,
        disk_usage=disk_usage,
    )
    guard.check("preflight")
    photon = _open_pinned(config.photon_dump, config.photon_pin, "Photon dump")
    overture: PinnedFile | None = None
    output_temp: pathlib.Path | None = None
    receipt_temp: pathlib.Path | None = None
    try:
        overture = _open_pinned(config.overture_csv, config.overture_pin, "Overture CSV")
        locality_values, locality_counts = load_localities(overture)
        if (
            locality_counts["overture_rows_scanned_for_localities"]
            != config.expected_overture_rows
        ):
            raise SupplementError(
                "Overture row-count pin mismatch: got "
                f"{locality_counts['overture_rows_scanned_for_localities']}, expected "
                f"{config.expected_overture_rows}"
            )
        resolver = LocalityResolver(locality_values, config.locality_max_distance_m)
        counts: defaultdict[str, int] = defaultdict(int, locality_counts)
        with tempfile.TemporaryDirectory(
            prefix=".de-photon-osm-supplement-", dir=config.temp_dir
        ) as owned:
            run_dir = pathlib.Path(owned)
            features, metadata = _iter_photon_features(
                photon,
                config.zstd_binary,
                counts,
                {
                    "version": config.expected_photon_version,
                    "database_version": config.expected_photon_database_version,
                    "data_timestamp": config.expected_photon_data_timestamp,
                },
            )
            chunk: list[dict[str, str]] = []
            runs: list[pathlib.Path] = []
            try:
                for feature in features:
                    counts["photon_features_seen"] += 1
                    row = _feature_to_run_row(feature, resolver, counts)
                    if row is None:
                        continue
                    counts["osm_candidate_rows"] += 1
                    chunk.append(row)
                    if len(chunk) >= config.chunk_rows:
                        guard.check("before-sorted-run")
                        runs.append(_write_run(chunk, run_dir, len(runs)))
                        counts["sorted_runs"] += 1
                        chunk = []
                        guard.check("after-sorted-run")
            finally:
                # A schema error in the consumer happens outside the generator
                # frame; close explicitly so its zstd child is always killed
                # and reaped before this function returns.
                close = getattr(features, "close", None)
                if close is not None:
                    close()
            if counts["jsonl_lines"] != config.expected_photon_jsonl_records:
                raise SupplementError(
                    "Photon JSONL record-count pin mismatch: "
                    f"got {counts['jsonl_lines']}, expected "
                    f"{config.expected_photon_jsonl_records}"
                )
            if (
                counts["jsonl_uncompressed_bytes"]
                != config.expected_photon_uncompressed_bytes
            ):
                raise SupplementError(
                    "Photon uncompressed-byte pin mismatch: "
                    f"got {counts['jsonl_uncompressed_bytes']}, expected "
                    f"{config.expected_photon_uncompressed_bytes}"
                )
            if counts["osm_candidate_rows"] != config.expected_osm_candidate_rows:
                raise SupplementError(
                    "OSM candidate-row pin mismatch: "
                    f"got {counts['osm_candidate_rows']}, expected "
                    f"{config.expected_osm_candidate_rows}"
                )
            if (
                counts["rejected_non_single_house"]
                != config.expected_rejected_non_single_house
            ):
                raise SupplementError(
                    "non-single-house rejection pin mismatch: "
                    f"got {counts['rejected_non_single_house']}, expected "
                    f"{config.expected_rejected_non_single_house}"
                )
            if chunk:
                guard.check("before-final-sorted-run")
                runs.append(_write_run(chunk, run_dir, len(runs)))
                counts["sorted_runs"] += 1
                guard.check("after-final-sorted-run")
            counts["hashed_localities"] = resolver.hashed_localities

            # The writer itself uses O_EXCL ("xb").  A random sibling name
            # avoids the insecure create-close-unlink-open sequence.
            output_temp = config.output_csv.parent / (
                f".{config.output_csv.name}.{uuid.uuid4().hex}.partial"
            )
            guard.check("before-output")
            consensus = _consensus_rows(_merged_runs(runs), counts)
            with _canonical_gzip_writer(output_temp, BUILDER_HEADER) as writer:
                _merge_output(overture, iter(consensus), writer, counts)
            guard.check("after-output")
            _recheck_pinned(photon, "Photon dump")
            _recheck_pinned(overture, "Overture CSV")
            output_evidence = _file_evidence(output_temp)
            output_evidence["path"] = str(config.output_csv)
            receipt_value: dict[str, Any] = {
                "schema": SCHEMA,
                "status": LAB_STATUS,
                "license": {
                    "data": ODBL_LICENSE,
                    "data_url": ODBL_LICENSE_URL,
                    "attribution": OSM_ATTRIBUTION,
                    "attribution_url": OSM_ATTRIBUTION_URL,
                    "shipping": "FORBIDDEN_WITHOUT_SEPARATE_OWNER_LEGAL_RELEASE_DECISION",
                },
                "policy": {
                    "network_calls": 0,
                    "photon_engine_calls": 0,
                    "gridpin_engine_calls": 0,
                    "full_uncompressed_dump_materialized": False,
                    "overture_nonblank_precedence": True,
                    "blank_postcode_fill_requires_osm_consensus": True,
                },
                "inputs": {
                    "photon_dump": dict(photon.evidence),
                    "overture_builder_csv": dict(overture.evidence),
                },
                "photon_metadata": metadata,
                "configuration": {
                    "chunk_rows": config.chunk_rows,
                    "expected_photon_jsonl_records": (
                        config.expected_photon_jsonl_records
                    ),
                    "expected_photon_uncompressed_bytes": (
                        config.expected_photon_uncompressed_bytes
                    ),
                    "expected_photon_version": config.expected_photon_version,
                    "expected_photon_database_version": (
                        config.expected_photon_database_version
                    ),
                    "expected_photon_data_timestamp": (
                        config.expected_photon_data_timestamp
                    ),
                    "expected_overture_rows": config.expected_overture_rows,
                    "expected_osm_candidate_rows": (
                        config.expected_osm_candidate_rows
                    ),
                    "expected_rejected_non_single_house": (
                        config.expected_rejected_non_single_house
                    ),
                    "max_jsonl_line_bytes": MAX_JSONL_LINE_BYTES,
                    "max_overture_identity_rows": MAX_OVERTURE_IDENTITY_ROWS,
                    "minimum_free_bytes": config.minimum_free_bytes,
                    "locality_max_distance_m": config.locality_max_distance_m,
                    "single_house_grammar": _SINGLE_HOUSE.pattern,
                    "builder_header": list(BUILDER_HEADER),
                },
                "counts": dict(sorted(counts.items())),
                "output": output_evidence,
            }
            receipt_bytes = canonical_json_bytes(receipt_value)
            fd, name = tempfile.mkstemp(
                prefix=f".{config.receipt.name}.",
                suffix=".partial",
                dir=config.receipt.parent,
            )
            receipt_temp = pathlib.Path(name)
            receipt_fd = fd
            try:
                os.fchmod(receipt_fd, 0o600)
                stream = os.fdopen(receipt_fd, "wb", closefd=True)
                receipt_fd = -1
                with stream:
                    stream.write(receipt_bytes)
                    stream.flush()
                    os.fsync(stream.fileno())
            finally:
                if receipt_fd >= 0:
                    os.close(receipt_fd)
            guard.check("before-commit")
            _commit_pair_no_replace(
                (
                    (output_temp, config.output_csv),
                    (receipt_temp, config.receipt),
                )
            )
            output_temp = None
            receipt_temp = None
            return receipt_value
    finally:
        photon.stream.close()
        if overture is not None:
            overture.stream.close()
        for temporary in (output_temp, receipt_temp):
            if temporary is not None:
                with contextlib.suppress(FileNotFoundError):
                    temporary.unlink()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--validate-photon-only",
        action="store_true",
        help="strictly scan every pinned JSONL record and write only the receipt",
    )
    parser.add_argument("--photon-dump", type=pathlib.Path, required=True)
    parser.add_argument("--expected-photon-sha256", required=True)
    parser.add_argument("--expected-photon-bytes", type=int, required=True)
    parser.add_argument("--expected-photon-jsonl-records", type=int, required=True)
    parser.add_argument("--expected-photon-uncompressed-bytes", type=int, required=True)
    parser.add_argument("--expected-photon-version", required=True)
    parser.add_argument("--expected-photon-database-version", required=True)
    parser.add_argument("--expected-photon-data-timestamp", required=True)
    parser.add_argument("--overture-build-csv", type=pathlib.Path)
    parser.add_argument("--expected-overture-sha256")
    parser.add_argument("--expected-overture-bytes", type=int)
    parser.add_argument("--expected-overture-rows", type=int)
    parser.add_argument("--expected-osm-candidate-rows", type=int)
    parser.add_argument(
        "--expected-rejected-non-single-house", type=int
    )
    parser.add_argument("--output-csv", type=pathlib.Path)
    parser.add_argument("--receipt", type=pathlib.Path, required=True)
    parser.add_argument(
        "--quarantine-dir",
        type=pathlib.Path,
        help="existing empty mode0700 directory required by --validate-photon-only",
    )
    parser.add_argument("--temp-dir", type=pathlib.Path)
    parser.add_argument("--chunk-rows", type=int, default=DEFAULT_CHUNK_ROWS)
    parser.add_argument("--minimum-free-bytes", type=int, default=DEFAULT_MIN_FREE_BYTES)
    parser.add_argument(
        "--locality-max-distance-m", type=float, default=DEFAULT_LOCALITY_DISTANCE_M
    )
    parser.add_argument("--zstd-binary", default="zstd")
    parser.add_argument(
        "--lab-only-odbl-not-shippable",
        action="store_true",
        help=f"required acknowledgement: output status is {LAB_STATUS}",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    if args.validate_photon_only:
        if args.quarantine_dir is None:
            parser.error("--validate-photon-only requires --quarantine-dir")
        validation = ValidationConfig(
            photon_dump=args.photon_dump,
            photon_pin=Pin(args.expected_photon_sha256, args.expected_photon_bytes),
            expected_photon_jsonl_records=args.expected_photon_jsonl_records,
            expected_photon_uncompressed_bytes=(
                args.expected_photon_uncompressed_bytes
            ),
            expected_photon_version=args.expected_photon_version,
            expected_photon_database_version=args.expected_photon_database_version,
            expected_photon_data_timestamp=args.expected_photon_data_timestamp,
            receipt=args.receipt,
            quarantine_dir=args.quarantine_dir,
            minimum_free_bytes=args.minimum_free_bytes,
            zstd_binary=args.zstd_binary,
        )
        try:
            validation_receipt = validate_photon_dump(validation)
        except SupplementError as exc:
            print(f"STOP: {exc}", file=os.sys.stderr)
            return 2
        print(json.dumps(validation_receipt, ensure_ascii=False, sort_keys=True))
        return 0 if validation_receipt["status"] == "PASS_STRICT_JSON_ALL_RECORDS" else 3

    required_build_args = {
        "--overture-build-csv": args.overture_build_csv,
        "--expected-overture-sha256": args.expected_overture_sha256,
        "--expected-overture-bytes": args.expected_overture_bytes,
        "--expected-overture-rows": args.expected_overture_rows,
        "--expected-osm-candidate-rows": args.expected_osm_candidate_rows,
        "--expected-rejected-non-single-house": (
            args.expected_rejected_non_single_house
        ),
        "--output-csv": args.output_csv,
        "--temp-dir": args.temp_dir,
    }
    missing = [name for name, value in required_build_args.items() if value is None]
    if missing:
        parser.error("build mode requires " + ", ".join(missing))
    config = Config(
        photon_dump=args.photon_dump,
        photon_pin=Pin(args.expected_photon_sha256, args.expected_photon_bytes),
        expected_photon_jsonl_records=args.expected_photon_jsonl_records,
        expected_photon_uncompressed_bytes=args.expected_photon_uncompressed_bytes,
        expected_photon_version=args.expected_photon_version,
        expected_photon_database_version=args.expected_photon_database_version,
        expected_photon_data_timestamp=args.expected_photon_data_timestamp,
        expected_overture_rows=args.expected_overture_rows,
        expected_osm_candidate_rows=args.expected_osm_candidate_rows,
        expected_rejected_non_single_house=(
            args.expected_rejected_non_single_house
        ),
        overture_csv=args.overture_build_csv,
        overture_pin=Pin(args.expected_overture_sha256, args.expected_overture_bytes),
        output_csv=args.output_csv,
        receipt=args.receipt,
        temp_dir=args.temp_dir,
        chunk_rows=args.chunk_rows,
        minimum_free_bytes=args.minimum_free_bytes,
        locality_max_distance_m=args.locality_max_distance_m,
        zstd_binary=args.zstd_binary,
        lab_acknowledgement=(
            LAB_STATUS if args.lab_only_odbl_not_shippable else ""
        ),
    )
    try:
        receipt = build_supplement(config)
    except SupplementError as exc:
        print(f"STOP: {exc}", file=os.sys.stderr)
        return 2
    print(json.dumps(receipt, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
