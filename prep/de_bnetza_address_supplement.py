#!/usr/bin/env python3
"""Build a deterministic, outcome-blind BNetzA address supplement for DE.

The closed Germany release and the OSM/Photon LAB corpus remain immutable.  This
module consumes the pinned complete BNetzA candidate frame and overlays its
safe physical-address projection on an explicitly pinned builder CSV.  It does
not select benchmark rows, call a geocoder, open the network, or choose the
first member of an ambiguous group.
"""

from __future__ import annotations

import argparse
import base64
from collections import defaultdict
from collections.abc import Iterable, Iterator, Mapping, Sequence
import csv
from dataclasses import dataclass, field
import hashlib
import json
import math
import os
import pathlib
import re
import shutil
import unicodedata
from typing import Any, BinaryIO, TextIO

import de_photon_osm_supplement as builder


SCHEMA = "gridpin-de-bnetza-address-supplement-v1"
SOURCE_CARD_SCHEMA = "gridpin-de-bnetza-source-card-v1"
FRAME_SCHEMA = "gridpin-de-bnetza-candidate-frame-v1"
GROUP_SCHEMA = "gridpin-de-bnetza-candidate-group-v1"
LICENSE = "CC BY 4.0"
ATTRIBUTION = "Bundesnetzagentur.de"
OVERLAY_ADD_AND_FILL = "ADD_AND_FILL"
OVERLAY_POSTCODE_FILL_ONLY = "POSTCODE_FILL_ONLY"
OVERLAY_MODES = frozenset({
    OVERLAY_ADD_AND_FILL,
    OVERLAY_POSTCODE_FILL_ONLY,
})
MAX_COORDINATE_SPREAD_M = 50.0
MAX_BUILDER_IDENTITY_ROWS = 100_000
DEFAULT_MIN_FREE_BYTES = 10 * 2**30
_POSTCODE = re.compile(r"[0-9]{5}")
_SHA256 = re.compile(r"[0-9a-f]{64}")
_STATUS = frozenset({
    "PUBLIC_PERMISSIVE_DEVELOPMENT",
    "ODBL_LAB_DEVELOPMENT",
})

STATE_CODES = {
    "Baden-Württemberg": "DE-BW",
    "Berlin": "DE-BE",
    "Brandenburg": "DE-BB",
    "Bremen": "DE-HB",
    "Hamburg": "DE-HH",
    "Hessen": "DE-HE",
    "Mecklenburg-Vorpommern": "DE-MV",
    "Niedersachsen": "DE-NI",
    "Nordrhein-Westfalen": "DE-NW",
    "Rheinland-Pfalz": "DE-RP",
    "Saarland": "DE-SL",
    "Sachsen": "DE-SN",
    "Sachsen-Anhalt": "DE-ST",
    "Schleswig-Holstein": "DE-SH",
    "Thüringen": "DE-TH",
}


class BNetzASupplementError(RuntimeError):
    """The supplement could not be built without weakening a guard."""


@dataclass(frozen=True)
class Pin:
    sha256: str
    bytes: int


@dataclass(frozen=True)
class SourceContract:
    raw_source: pathlib.Path
    raw_pin: Pin
    terms_snapshot: pathlib.Path
    terms_pin: Pin
    source_url: str
    terms_url: str
    snapshot_date: str
    embedded_update: str
    license: str = LICENSE
    attribution: str = ATTRIBUTION


@dataclass(frozen=True)
class Config:
    source: SourceContract
    candidate_frame: pathlib.Path
    candidate_frame_pin: Pin
    builder_csv: pathlib.Path
    builder_pin: Pin
    expected_builder_rows: int
    expected_candidate_groups: int
    output_csv: pathlib.Path
    receipt: pathlib.Path
    status: str
    overlay_mode: str
    minimum_free_bytes: int = DEFAULT_MIN_FREE_BYTES


@dataclass(frozen=True)
class Coordinate:
    lat: float
    lon: float


@dataclass(frozen=True)
class Projection:
    street_norm: str
    city_norm: str
    number: int
    suffix: str
    postcode: str
    state: str
    state_code: str
    street_display: str
    city_display: str
    coordinates: tuple[Coordinate, ...]
    source_groups: int = 1

    @property
    def semantic_key(self) -> tuple[str, str, int, str]:
        return (self.street_norm, self.city_norm, self.number, self.suffix)

    @property
    def source_key(self) -> tuple[str, str, int, str, str, str]:
        return (*self.semantic_key, self.postcode, self.state_code)


@dataclass
class Locality:
    code: str
    norm: str
    display: str
    province: str
    postcodes: set[str] = field(default_factory=set)


@dataclass
class AddressObservation:
    codes: set[str] = field(default_factory=set)
    postcodes: set[str] = field(default_factory=set)
    blank_postcode: bool = False


@dataclass
class BaseContext:
    localities: dict[str, Locality]
    address_observations: dict[tuple[str, str, int, str], AddressObservation]
    used_codes: set[str]
    rows: int


@dataclass(frozen=True)
class ResolvedProjection:
    identity: tuple[str, str, int, str]
    row: dict[str, str]
    source: Projection
    locality_mode: str


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
        raise BNetzASupplementError(f"{label} SHA-256 pin is invalid")
    if isinstance(pin.bytes, bool) or pin.bytes <= 0:
        raise BNetzASupplementError(f"{label} byte pin must be positive")


def _as_builder_pin(pin: Pin) -> builder.Pin:
    return builder.Pin(sha256=pin.sha256, bytes=pin.bytes)


def _open_pinned(path: pathlib.Path, pin: Pin, label: str) -> builder.PinnedFile:
    _validate_pin(pin, label)
    try:
        return builder._open_pinned(path, _as_builder_pin(pin), label)
    except builder.SupplementError as exc:
        raise BNetzASupplementError(str(exc)) from exc


def _recheck_pinned(pinned: builder.PinnedFile, label: str) -> None:
    try:
        builder._recheck_pinned(pinned, label)
    except builder.SupplementError as exc:
        raise BNetzASupplementError(str(exc)) from exc


def _strict_json(raw: str, label: str) -> Any:
    try:
        return builder.strict_json_loads(raw)
    except (ValueError, json.JSONDecodeError) as exc:
        raise BNetzASupplementError(f"{label}: invalid strict JSON: {exc}") from exc


def _plain_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip() or "\x00" in value:
        raise BNetzASupplementError(f"{label} must be a non-empty string")
    return value.strip()


def _normalized_forms(values: Any, label: str) -> tuple[str, str]:
    if not isinstance(values, list) or not values:
        raise BNetzASupplementError(f"{label} must contain source forms")
    literal = {_plain_string(value, label) for value in values}
    normalized = {builder.normalize_text(value) for value in literal}
    if "" in normalized or len(normalized) != 1:
        raise BNetzASupplementError(f"{label} has conflicting normalized projections")
    display = min(literal, key=lambda value: (unicodedata.normalize("NFKC", value), value))
    return next(iter(normalized)), display


def _house_projection(values: Any) -> tuple[int, str]:
    if not isinstance(values, list) or not values:
        raise BNetzASupplementError("house source forms must be non-empty")
    parsed = {builder.parse_single_house(value) for value in values}
    if None in parsed or len(parsed) != 1:
        raise BNetzASupplementError("house source forms are not one exact single house")
    result = next(iter(parsed))
    if result is None or result[0] <= 0:
        raise BNetzASupplementError("house number must be positive")
    return result


def _coordinate(value: Any) -> Coordinate:
    if not isinstance(value, dict) or set(value) != {"lat", "lon"}:
        raise BNetzASupplementError("coordinate projection must contain only lat/lon")
    try:
        lat = float(value["lat"])
        lon = float(value["lon"])
    except (TypeError, ValueError) as exc:
        raise BNetzASupplementError("coordinate projection is not numeric") from exc
    if not math.isfinite(lat) or not math.isfinite(lon):
        raise BNetzASupplementError("coordinate projection must be finite")
    if not (47 <= lat <= 56 and 5 <= lon <= 16):
        raise BNetzASupplementError("coordinate projection is outside the DE guard box")
    return Coordinate(lat=lat, lon=lon)


def _haversine(first: Coordinate, second: Coordinate) -> float:
    radius = 6_371_008.8
    lat1, lat2 = math.radians(first.lat), math.radians(second.lat)
    delta_lat = lat2 - lat1
    delta_lon = math.radians(second.lon - first.lon)
    value = (
        math.sin(delta_lat / 2) ** 2
        + math.cos(lat1) * math.cos(lat2) * math.sin(delta_lon / 2) ** 2
    )
    return 2 * radius * math.asin(min(1.0, math.sqrt(value)))


def _coordinate_spread(values: Sequence[Coordinate]) -> float:
    maximum = 0.0
    for index, first in enumerate(values):
        for second in values[index + 1 :]:
            maximum = max(maximum, _haversine(first, second))
    return maximum


def _medoid(values: Sequence[Coordinate]) -> Coordinate:
    if not values:
        raise BNetzASupplementError("coordinate group must not be empty")
    distinct = tuple(sorted(set(values), key=lambda item: (item.lat, item.lon)))
    if _coordinate_spread(distinct) > MAX_COORDINATE_SPREAD_M:
        raise BNetzASupplementError("coordinate group exceeds 50 metre consensus")
    return min(
        distinct,
        key=lambda candidate: (
            max(_haversine(candidate, other) for other in distinct),
            sum(_haversine(candidate, other) for other in distinct),
            candidate.lat,
            candidate.lon,
        ),
    )


def project_candidate_group(record: Mapping[str, Any]) -> Projection:
    if record.get("record_type") != "candidate_group" or record.get("schema") != GROUP_SCHEMA:
        raise BNetzASupplementError("record is not a BNetzA candidate group")
    if record.get("outcome_blind") is not True:
        raise BNetzASupplementError("candidate group is not outcome-blind")
    ambiguity = record.get("coordinate_ambiguity")
    if not isinstance(ambiguity, dict) or not isinstance(ambiguity.get("ambiguous"), bool):
        raise BNetzASupplementError("candidate group coordinate ambiguity is malformed")
    if ambiguity["ambiguous"]:
        raise BNetzASupplementError("candidate group is coordinate-ambiguous")
    address = record.get("address")
    if not isinstance(address, dict):
        raise BNetzASupplementError("candidate group address is malformed")
    street_norm, street_display = _normalized_forms(
        address.get("street_source_forms"), "street source forms"
    )
    city_norm, city_display = _normalized_forms(
        address.get("city_source_forms"), "city source forms"
    )
    number, suffix = _house_projection(address.get("house_number_source_forms"))
    postcodes = address.get("postcode_source_forms")
    if not isinstance(postcodes, list):
        raise BNetzASupplementError("postcode source forms are malformed")
    values = {_plain_string(value, "postcode source form") for value in postcodes}
    if len(values) != 1 or _POSTCODE.fullmatch(next(iter(values))) is None:
        raise BNetzASupplementError("postcode source forms do not agree on one full PLZ")
    postcode = next(iter(values))
    state = _plain_string(record.get("state"), "state")
    try:
        state_code = STATE_CODES[state]
    except KeyError as exc:
        raise BNetzASupplementError(f"unsupported BNetzA state: {state!r}") from exc
    raw_coordinates = record.get("coordinates")
    if not isinstance(raw_coordinates, list) or not raw_coordinates:
        raise BNetzASupplementError("candidate group coordinates are empty")
    coordinates = tuple(_coordinate(value) for value in raw_coordinates)
    declared_spread = record.get("coordinate_spread_m")
    try:
        declared = float(declared_spread)
    except (TypeError, ValueError) as exc:
        raise BNetzASupplementError("declared coordinate spread is invalid") from exc
    observed = _coordinate_spread(coordinates)
    if declared < 0 or declared > MAX_COORDINATE_SPREAD_M or observed > MAX_COORDINATE_SPREAD_M:
        raise BNetzASupplementError("candidate coordinate spread exceeds the accepted bound")
    if abs(declared - observed) > 0.01:
        raise BNetzASupplementError("candidate coordinate spread does not reproduce")
    return Projection(
        street_norm=street_norm,
        city_norm=city_norm,
        number=number,
        suffix=suffix,
        postcode=postcode,
        state=builder.normalize_text(state),
        state_code=state_code,
        street_display=street_display,
        city_display=city_display,
        coordinates=coordinates,
    )


def _merge_projection_group(values: Sequence[Projection]) -> Projection:
    first = values[0]
    if any(value.source_key != first.source_key for value in values):
        raise BNetzASupplementError("source projection has conflicting semantic members")
    coordinates = tuple(value for projection in values for value in projection.coordinates)
    _medoid(coordinates)
    return Projection(
        street_norm=first.street_norm,
        city_norm=first.city_norm,
        number=first.number,
        suffix=first.suffix,
        postcode=first.postcode,
        state=first.state,
        state_code=first.state_code,
        street_display=min(value.street_display for value in values),
        city_display=min(value.city_display for value in values),
        coordinates=coordinates,
        source_groups=sum(value.source_groups for value in values),
    )


def load_projections(
    pinned: builder.PinnedFile,
    *,
    expected_candidate_groups: int,
) -> tuple[tuple[Projection, ...], dict[str, int], Mapping[str, Any]]:
    pinned.stream.seek(0)
    text = TextIOWrapperNoClose(pinned.stream)
    counts: defaultdict[str, int] = defaultdict(int)
    groups: dict[tuple[str, str, int, str, str, str], list[Projection]] = defaultdict(list)
    metadata: Mapping[str, Any] | None = None
    previous_type = "candidate_frame"
    try:
        for line_number, raw in enumerate(text, 1):
            if len(raw.encode("utf-8")) > 16 * 2**20:
                raise BNetzASupplementError("candidate-frame JSONL line exceeds 16 MiB")
            value = _strict_json(raw, f"candidate-frame line {line_number}")
            if not isinstance(value, dict):
                raise BNetzASupplementError("candidate-frame record must be an object")
            record_type = value.get("record_type")
            if line_number == 1:
                if record_type != "candidate_frame" or value.get("schema") != FRAME_SCHEMA:
                    raise BNetzASupplementError("candidate-frame metadata schema drift")
                if value.get("outcome_blind") is not True:
                    raise BNetzASupplementError("candidate-frame metadata is not outcome-blind")
                metadata = value
                continue
            if record_type == "candidate_group":
                if previous_type == "excluded_group":
                    raise BNetzASupplementError("candidate groups appear after exclusions")
                counts["candidate_groups"] += 1
                try:
                    projection = project_candidate_group(value)
                except BNetzASupplementError as exc:
                    reason = str(exc)
                    if reason == "candidate group is coordinate-ambiguous":
                        counts["excluded_coordinate_ambiguity"] += 1
                    elif "house" in reason:
                        counts["excluded_house_projection"] += 1
                    elif "normalized projections" in reason or "source forms" in reason:
                        counts["excluded_source_projection"] += 1
                    else:
                        raise
                    continue
                additions = value["address"].get("address_addition_source_forms")
                if isinstance(additions, list) and any(str(item).strip() for item in additions):
                    counts["accepted_address_addition_ignored"] += 1
                groups[projection.source_key].append(projection)
                counts["projected_candidate_groups"] += 1
                previous_type = "candidate_group"
            elif record_type == "excluded_group":
                counts["upstream_excluded_groups"] += 1
                previous_type = "excluded_group"
            else:
                raise BNetzASupplementError(f"unexpected candidate-frame record type {record_type!r}")
    finally:
        text.detach()
    if metadata is None:
        raise BNetzASupplementError("candidate-frame metadata is missing")
    if counts["candidate_groups"] != expected_candidate_groups:
        raise BNetzASupplementError(
            f"candidate-group count mismatch: got {counts['candidate_groups']}, "
            f"expected {expected_candidate_groups}"
        )
    projected: list[Projection] = []
    for key in sorted(groups):
        values = groups[key]
        try:
            projected.append(_merge_projection_group(values))
        except BNetzASupplementError:
            counts["excluded_duplicate_projection_conflict"] += len(values)
            continue
        counts["duplicate_source_groups_collapsed"] += len(values) - 1
    by_semantic: dict[tuple[str, str, int, str], list[Projection]] = defaultdict(list)
    for value in projected:
        by_semantic[value.semantic_key].append(value)
    accepted: list[Projection] = []
    for key in sorted(by_semantic):
        values = by_semantic[key]
        signatures = {(value.postcode, value.state_code) for value in values}
        if len(signatures) != 1:
            counts["excluded_semantic_postcode_or_state_conflict"] += len(values)
            continue
        accepted.extend(values)
    counts["accepted_source_projections"] = len(accepted)
    if not accepted:
        raise BNetzASupplementError("candidate frame produced no safe source projections")
    return tuple(accepted), dict(sorted(counts.items())), metadata


class TextIOWrapperNoClose:
    """Strict UTF-8 iterator whose detach preserves the caller-owned binary fd."""

    def __init__(self, stream: BinaryIO) -> None:
        import io

        self._text = io.TextIOWrapper(stream, encoding="utf-8", errors="strict", newline="")

    def __iter__(self) -> Iterator[str]:
        return iter(self._text)

    def detach(self) -> BinaryIO:
        return self._text.detach()


def _csv_reader(pinned: builder.PinnedFile) -> tuple[TextIO, csv.DictReader]:
    pinned.stream.seek(0)
    try:
        text, reader = builder._csv_text_reader(
            pinned.stream, gzipped=pinned.path.suffix.lower() == ".gz"
        )
    except builder.SupplementError as exc:
        raise BNetzASupplementError(str(exc)) from exc
    if tuple(reader.fieldnames or ()) != builder.BUILDER_HEADER:
        text.close()
        raise BNetzASupplementError("builder CSV header drift")
    return text, reader


def _row_identity(row: Mapping[str, str]) -> tuple[str, str, int, str]:
    try:
        number = int(row["numero"])
    except (KeyError, TypeError, ValueError) as exc:
        raise BNetzASupplementError("builder row has invalid house number") from exc
    if number < 0 or number > 0xFFFF_FFFF:
        raise BNetzASupplementError("builder row house number is out of range")
    return row["nom_voie_norm"], row["code_insee"], number, row["rep"]


def _row_sort_key(row: Mapping[str, str]) -> tuple[Any, ...]:
    try:
        lon, lat = float(row["lon"]), float(row["lat"])
    except (KeyError, TypeError, ValueError) as exc:
        raise BNetzASupplementError("builder row has invalid coordinates") from exc
    if not math.isfinite(lon) or not math.isfinite(lat):
        raise BNetzASupplementError("builder row has non-finite coordinates")
    return _row_identity(row) + (lon, lat)


def load_base_context(
    pinned: builder.PinnedFile,
    projections: Sequence[Projection],
    *,
    expected_rows: int,
) -> BaseContext:
    semantics = {value.semantic_key for value in projections}
    localities: dict[str, Locality] = {}
    observations: dict[tuple[str, str, int, str], AddressObservation] = {}
    previous: tuple[Any, ...] | None = None
    rows = 0
    text, reader = _csv_reader(pinned)
    try:
        for row in reader:
            rows += 1
            sort_key = _row_sort_key(row)
            if previous is not None and sort_key < previous:
                raise BNetzASupplementError("builder CSV is not canonically sorted")
            previous = sort_key
            code = _plain_string(row["code_insee"], "builder locality code")
            if len(code.encode("utf-8")) > 8:
                raise BNetzASupplementError("builder locality code exceeds eight bytes")
            norm = _plain_string(row["nom_commune_norm"], "builder locality norm")
            display = _plain_string(row["nom_commune"], "builder locality display")
            province = row["provincia_norm"].strip()
            locality = localities.get(code)
            if locality is None:
                locality = Locality(code=code, norm=norm, display=display, province=province)
                localities[code] = locality
            elif locality.norm != norm:
                raise BNetzASupplementError(f"builder locality {code} maps to two names")
            elif locality.province and province and locality.province != province:
                raise BNetzASupplementError(f"builder locality {code} maps to two provinces")
            elif province:
                locality.province = province
            postcode = builder._postcode(row)
            if postcode:
                locality.postcodes.add(postcode)
            semantic = (row["nom_voie_norm"], norm, int(row["numero"]), row["rep"])
            if semantic in semantics:
                observed = observations.setdefault(semantic, AddressObservation())
                observed.codes.add(code)
                if postcode:
                    observed.postcodes.add(postcode)
                else:
                    observed.blank_postcode = True
    finally:
        text.close()
    if rows != expected_rows:
        raise BNetzASupplementError(
            f"builder row-count pin mismatch: got {rows}, expected {expected_rows}"
        )
    return BaseContext(
        localities=localities,
        address_observations=observations,
        used_codes=set(localities),
        rows=rows,
    )


class ExactLocalityResolver:
    def __init__(self, context: BaseContext) -> None:
        self.context = context
        self._hash_to_key: dict[str, str] = {}
        self._key_to_hash: dict[str, str] = {}

    def _hashed(self, projection: Projection) -> str:
        key = "\x1f".join((projection.state_code, projection.postcode, projection.city_norm))
        existing = self._key_to_hash.get(key)
        if existing is not None:
            return existing
        digest = hashlib.sha256(key.encode("utf-8")).digest()
        code = "R" + base64.b32encode(digest).decode("ascii")[:7]
        other = self._hash_to_key.get(code)
        if other is not None and other != key:
            raise BNetzASupplementError(f"registry locality hash collision for {code}")
        if code in self.context.used_codes:
            raise BNetzASupplementError(f"registry locality {code} collides with base")
        self._hash_to_key[code] = key
        self._key_to_hash[key] = code
        self.context.used_codes.add(code)
        return code

    def resolve(self, projection: Projection) -> tuple[Locality, str]:
        observation = self.context.address_observations.get(projection.semantic_key)
        if observation is not None:
            if observation.postcodes - {projection.postcode}:
                raise BNetzASupplementError("exact base address has a conflicting full postcode")
            if len(observation.codes) != 1:
                raise BNetzASupplementError("exact base address maps to multiple localities")
            code = next(iter(observation.codes))
            return self.context.localities[code], "exact_address"
        city = [value for value in self.context.localities.values() if value.norm == projection.city_norm]
        compatible = [
            value
            for value in city
            if not value.province or value.province == projection.state
        ]
        postcode = [value for value in compatible if projection.postcode in value.postcodes]
        if len(postcode) == 1:
            return postcode[0], "exact_city_postcode"
        if len(postcode) > 1:
            raise BNetzASupplementError("city/postcode maps to multiple base localities")
        state_known = [value for value in compatible if value.province == projection.state]
        if len(state_known) == 1:
            return state_known[0], "exact_city_state"
        if len(state_known) > 1:
            raise BNetzASupplementError("city/state maps to multiple base localities")
        if len(city) == 1:
            return city[0], "unique_city"
        if compatible:
            raise BNetzASupplementError("city maps to ambiguous base localities")
        code = self._hashed(projection)
        return Locality(
            code=code,
            norm=projection.city_norm,
            display=projection.city_display,
            province=projection.state,
            postcodes={projection.postcode},
        ), "hashed_new_city"

    @property
    def hashed_localities(self) -> int:
        return len(self._key_to_hash)


def resolve_projections(
    projections: Sequence[Projection], context: BaseContext
) -> tuple[tuple[ResolvedProjection, ...], dict[str, int]]:
    resolver = ExactLocalityResolver(context)
    counts: defaultdict[str, int] = defaultdict(int)
    grouped: dict[tuple[str, str, int, str], list[ResolvedProjection]] = defaultdict(list)
    for projection in projections:
        try:
            locality, mode = resolver.resolve(projection)
        except BNetzASupplementError as exc:
            counts["excluded_locality_or_postcode_conflict"] += 1
            counts["excluded_" + hashlib.sha256(str(exc).encode()).hexdigest()[:12]] += 1
            continue
        coordinate = _medoid(projection.coordinates)
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
        identity = _row_identity(row)
        grouped[identity].append(
            ResolvedProjection(identity=identity, row=row, source=projection, locality_mode=mode)
        )
        counts["locality_" + mode] += 1
    resolved: list[ResolvedProjection] = []
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
        if len(signatures) != 1 or _coordinate_spread(coordinates) > MAX_COORDINATE_SPREAD_M:
            counts["excluded_resolved_projection_conflict"] += len(values)
            continue
        chosen = min(
            values,
            key=lambda value: (
                _medoid(coordinates).lat,
                _medoid(coordinates).lon,
                value.row["nom_voie"],
                value.row["nom_commune"],
            ),
        )
        coordinate = _medoid(coordinates)
        row = dict(chosen.row)
        row["lon"] = format(coordinate.lon, ".15g")
        row["lat"] = format(coordinate.lat, ".15g")
        resolved.append(
            ResolvedProjection(
                identity=identity,
                row=row,
                source=chosen.source,
                locality_mode=chosen.locality_mode,
            )
        )
        counts["resolved_duplicates_collapsed"] += len(values) - 1
    counts["hashed_localities"] = resolver.hashed_localities
    counts["resolved_source_rows"] = len(resolved)
    if not resolved:
        raise BNetzASupplementError("locality arbitration rejected every source projection")
    return tuple(resolved), dict(sorted(counts.items()))


def _overlay_is_nonvacuous(
    projections: Sequence[ResolvedProjection],
    context: BaseContext,
    overlay_mode: str,
) -> bool:
    for projection in projections:
        observation = context.address_observations.get(projection.source.semantic_key)
        if observation is None:
            if overlay_mode == OVERLAY_ADD_AND_FILL:
                return True
            continue
        if observation.blank_postcode and (
            not observation.postcodes
            or observation.postcodes == {projection.row["code_postal_display"]}
        ):
            return True
    return False


def _select_projections_for_overlay(
    projections: Sequence[Projection],
    context: BaseContext,
    overlay_mode: str,
) -> tuple[tuple[Projection, ...], dict[str, int]]:
    if overlay_mode == OVERLAY_ADD_AND_FILL:
        return tuple(projections), {
            "source_projections_skipped_before_locality_resolution_by_policy": 0,
        }
    selected = tuple(
        projection
        for projection in projections
        if projection.semantic_key in context.address_observations
    )
    skipped = len(projections) - len(selected)
    if not selected:
        raise BNetzASupplementError("supplement is vacuous")
    return selected, {
        "source_projections_skipped_before_locality_resolution_by_policy": skipped,
    }


def _iter_builder_groups(
    pinned: builder.PinnedFile,
) -> Iterator[tuple[tuple[str, str, int, str], list[dict[str, str]]]]:
    text, reader = _csv_reader(pinned)
    current: tuple[str, str, int, str] | None = None
    rows: list[dict[str, str]] = []
    previous: tuple[Any, ...] | None = None
    try:
        for row in reader:
            sort_key = _row_sort_key(row)
            if previous is not None and sort_key < previous:
                raise BNetzASupplementError("builder CSV changed sort order between passes")
            previous = sort_key
            identity = _row_identity(row)
            if current is not None and identity != current:
                yield current, rows
                rows = []
            current = identity
            rows.append(row)
            if len(rows) > MAX_BUILDER_IDENTITY_ROWS:
                raise BNetzASupplementError("builder identity exceeds bounded row limit")
        if current is not None:
            yield current, rows
    finally:
        text.close()


def merge_output(
    pinned: builder.PinnedFile,
    projections: Sequence[ResolvedProjection],
    writer: csv.DictWriter,
    *,
    overlay_mode: str,
) -> dict[str, int]:
    counts: defaultdict[str, int] = defaultdict(int)
    counts["source_rows_added"] = 0
    counts["source_only_rows_skipped_by_policy"] = 0
    counts["base_blank_postcode_rows_filled"] = 0
    base_iter = iter(_iter_builder_groups(pinned))
    source_iter = iter(projections)
    base_value = next(base_iter, None)
    source_value = next(source_iter, None)
    while base_value is not None or source_value is not None:
        if source_value is None or (
            base_value is not None and base_value[0] < source_value.identity
        ):
            for row in base_value[1]:
                writer.writerow(row)
                counts["base_rows_retained"] += 1
                counts["output_rows"] += 1
            base_value = next(base_iter, None)
            continue
        if base_value is None or source_value.identity < base_value[0]:
            if overlay_mode == OVERLAY_POSTCODE_FILL_ONLY:
                counts["source_only_rows_skipped_by_policy"] += 1
            else:
                writer.writerow(source_value.row)
                counts["source_rows_added"] += 1
                counts["output_rows"] += 1
            source_value = next(source_iter, None)
            continue
        base_rows = base_value[1]
        known = {builder._postcode(row) for row in base_rows} - {""}
        source_postcode = source_value.row["code_postal_display"]
        if not known:
            for row in base_rows:
                projected = dict(row)
                projected["code_postal"] = source_postcode
                projected["code_postal_display"] = source_postcode
                writer.writerow(projected)
                counts["base_blank_postcode_rows_filled"] += 1
                counts["base_rows_retained"] += 1
                counts["output_rows"] += 1
            counts["source_rows_consumed_by_base_identity"] += 1
        elif known == {source_postcode}:
            for row in base_rows:
                if builder._postcode(row):
                    projected = row
                else:
                    projected = dict(row)
                    projected["code_postal"] = source_postcode
                    projected["code_postal_display"] = source_postcode
                    counts["base_blank_postcode_rows_filled"] += 1
                writer.writerow(projected)
                counts["base_rows_retained"] += 1
                counts["output_rows"] += 1
            counts["source_rows_already_present"] += 1
        else:
            for row in base_rows:
                writer.writerow(row)
                counts["base_rows_retained"] += 1
                counts["output_rows"] += 1
            counts["source_rows_quarantined_postcode_conflict"] += 1
        base_value = next(base_iter, None)
        source_value = next(source_iter, None)
    return dict(sorted(counts.items()))


def _file_evidence(path: pathlib.Path) -> dict[str, Any]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        while block := handle.read(8 * 2**20):
            digest.update(block)
            size += len(block)
    return {"path": str(path), "bytes": size, "sha256": digest.hexdigest()}


def _validate_source_contract(
    metadata: Mapping[str, Any], config: Config
) -> None:
    source = metadata.get("source")
    if not isinstance(source, dict):
        raise BNetzASupplementError("candidate-frame source metadata is missing")
    expected = config.source
    checks = {
        "url": expected.source_url,
        "snapshot_date": expected.snapshot_date,
        "embedded_update": expected.embedded_update,
        "license": expected.license,
        "attribution": expected.attribution.lower(),
        "sha256_pre": expected.raw_pin.sha256,
        "sha256_post": expected.raw_pin.sha256,
        "bytes_pre": expected.raw_pin.bytes,
        "bytes_post": expected.raw_pin.bytes,
    }
    for key, value in checks.items():
        observed = source.get(key)
        if key == "attribution" and isinstance(observed, str):
            observed = observed.lower()
        if observed != value:
            raise BNetzASupplementError(f"candidate-frame source metadata drift at {key}")


def _validate_config(config: Config) -> None:
    for label, pin in (
        ("raw source", config.source.raw_pin),
        ("terms snapshot", config.source.terms_pin),
        ("candidate frame", config.candidate_frame_pin),
        ("builder CSV", config.builder_pin),
    ):
        _validate_pin(pin, label)
    if config.source.license != LICENSE or config.source.attribution != ATTRIBUTION:
        raise BNetzASupplementError("BNetzA source contract license/attribution drift")
    if not config.source.source_url.startswith("https://") or not config.source.terms_url.startswith("https://"):
        raise BNetzASupplementError("source contract URLs must use HTTPS")
    if config.status not in _STATUS:
        raise BNetzASupplementError("unknown output status")
    if config.overlay_mode not in OVERLAY_MODES:
        raise BNetzASupplementError("unknown overlay mode")
    if config.expected_builder_rows <= 0 or config.expected_candidate_groups <= 0:
        raise BNetzASupplementError("expected row counts must be positive")
    if config.minimum_free_bytes < 0:
        raise BNetzASupplementError("minimum free bytes must be non-negative")
    for path in (config.output_csv, config.receipt):
        if path.exists():
            raise BNetzASupplementError(f"output must not exist: {path}")
        if not path.parent.is_dir():
            raise BNetzASupplementError(f"output parent is missing: {path.parent}")


def build(config: Config) -> dict[str, Any]:
    """Build the additive CSV and write-once receipt."""

    _validate_config(config)
    free = shutil.disk_usage(config.output_csv.parent).free
    if free < config.minimum_free_bytes:
        raise BNetzASupplementError(
            f"disk floor crossed before build: {free} < {config.minimum_free_bytes}"
        )
    raw = _open_pinned(config.source.raw_source, config.source.raw_pin, "raw BNetzA source")
    terms: builder.PinnedFile | None = None
    frame: builder.PinnedFile | None = None
    base: builder.PinnedFile | None = None
    try:
        terms = _open_pinned(config.source.terms_snapshot, config.source.terms_pin, "BNetzA terms snapshot")
        frame = _open_pinned(config.candidate_frame, config.candidate_frame_pin, "BNetzA candidate frame")
        base = _open_pinned(config.builder_csv, config.builder_pin, "builder CSV")
        projections, source_counts, metadata = load_projections(
            frame, expected_candidate_groups=config.expected_candidate_groups
        )
        _validate_source_contract(metadata, config)
        context = load_base_context(
            base, projections, expected_rows=config.expected_builder_rows
        )
        selected, selection_counts = _select_projections_for_overlay(
            projections,
            context,
            config.overlay_mode,
        )
        resolved, resolution_counts = resolve_projections(selected, context)
        if not _overlay_is_nonvacuous(resolved, context, config.overlay_mode):
            raise BNetzASupplementError("supplement is vacuous")
        with builder._canonical_gzip_writer(config.output_csv, builder.BUILDER_HEADER) as writer:
            merge_counts = merge_output(
                base,
                resolved,
                writer,
                overlay_mode=config.overlay_mode,
            )
        _recheck_pinned(raw, "raw BNetzA source")
        _recheck_pinned(terms, "BNetzA terms snapshot")
        _recheck_pinned(frame, "BNetzA candidate frame")
        _recheck_pinned(base, "builder CSV")
        counts = dict(
            sorted(
                {
                    **source_counts,
                    **selection_counts,
                    **resolution_counts,
                    **merge_counts,
                }.items()
            )
        )
        if config.overlay_mode == OVERLAY_POSTCODE_FILL_ONLY:
            changed = counts.get("base_blank_postcode_rows_filled", 0) > 0
        else:
            changed = (
                counts.get("source_rows_added", 0)
                + counts.get("base_blank_postcode_rows_filled", 0)
                > 0
            )
        if not changed:
            raise BNetzASupplementError("overlay preflight/merge invariant failed")
        if config.overlay_mode == OVERLAY_POSTCODE_FILL_ONLY:
            if counts.get("source_rows_added", 0) != 0:
                raise BNetzASupplementError("fill-only overlay added a source address point")
            if counts.get("output_rows", 0) != context.rows:
                raise BNetzASupplementError("fill-only overlay changed the builder row count")
            if counts.get("base_rows_retained", 0) != context.rows:
                raise BNetzASupplementError("fill-only overlay dropped a base row")
        receipt: dict[str, Any] = {
            "schema": SCHEMA,
            "status": config.status,
            "license": {
                "data": config.source.license,
                "license_url": config.source.terms_url,
                "attribution": config.source.attribution,
                "raw_source_url": config.source.source_url,
                "snapshot_date": config.source.snapshot_date,
            },
            "policy": {
                "outcome_blind_full_source": True,
                "network_calls_during_build": 0,
                "gridpin_engine_calls_during_build": 0,
                "photon_engine_calls_during_build": 0,
                "coordinate_ambiguity_fail_closed": True,
                "single_house_only": True,
                "exact_locality_or_collision_checked_hash": True,
                "base_nonblank_postcode_precedence": True,
                "overlay_mode": config.overlay_mode,
                "source_only_address_points_admitted": (
                    config.overlay_mode == OVERLAY_ADD_AND_FILL
                ),
                "base_coordinates_preserved": (
                    config.overlay_mode == OVERLAY_POSTCODE_FILL_ONLY
                ),
                "source_coordinates_written_to_output": (
                    config.overlay_mode == OVERLAY_ADD_AND_FILL
                ),
                "existing_base_identity_postcode_metadata_only": (
                    config.overlay_mode == OVERLAY_POSTCODE_FILL_ONLY
                ),
                "first_candidate_selection": False,
            },
            "configuration": {
                "expected_builder_rows": config.expected_builder_rows,
                "expected_candidate_groups": config.expected_candidate_groups,
                "overlay_mode": config.overlay_mode,
                "maximum_coordinate_spread_m": MAX_COORDINATE_SPREAD_M,
                "minimum_free_bytes": config.minimum_free_bytes,
                "builder_header": list(builder.BUILDER_HEADER),
            },
            "inputs": {
                "raw_source": dict(raw.evidence),
                "terms_snapshot": dict(terms.evidence),
                "candidate_frame": dict(frame.evidence),
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
        raw.stream.close()
        for pinned in (terms, frame, base):
            if pinned is not None:
                pinned.stream.close()


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--raw-source", type=pathlib.Path, required=True)
    parser.add_argument("--raw-sha256", required=True)
    parser.add_argument("--raw-bytes", type=int, required=True)
    parser.add_argument("--terms-snapshot", type=pathlib.Path, required=True)
    parser.add_argument("--terms-sha256", required=True)
    parser.add_argument("--terms-bytes", type=int, required=True)
    parser.add_argument("--source-url", required=True)
    parser.add_argument("--terms-url", required=True)
    parser.add_argument("--snapshot-date", required=True)
    parser.add_argument("--embedded-update", required=True)
    parser.add_argument("--candidate-frame", type=pathlib.Path, required=True)
    parser.add_argument("--candidate-frame-sha256", required=True)
    parser.add_argument("--candidate-frame-bytes", type=int, required=True)
    parser.add_argument("--builder-csv", type=pathlib.Path, required=True)
    parser.add_argument("--builder-sha256", required=True)
    parser.add_argument("--builder-bytes", type=int, required=True)
    parser.add_argument("--expected-builder-rows", type=int, required=True)
    parser.add_argument("--expected-candidate-groups", type=int, required=True)
    parser.add_argument("--output-csv", type=pathlib.Path, required=True)
    parser.add_argument("--receipt", type=pathlib.Path, required=True)
    parser.add_argument("--status", choices=sorted(_STATUS), required=True)
    parser.add_argument("--overlay-mode", choices=sorted(OVERLAY_MODES), required=True)
    parser.add_argument("--minimum-free-bytes", type=int, default=DEFAULT_MIN_FREE_BYTES)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    config = Config(
        source=SourceContract(
            raw_source=args.raw_source,
            raw_pin=Pin(args.raw_sha256, args.raw_bytes),
            terms_snapshot=args.terms_snapshot,
            terms_pin=Pin(args.terms_sha256, args.terms_bytes),
            source_url=args.source_url,
            terms_url=args.terms_url,
            snapshot_date=args.snapshot_date,
            embedded_update=args.embedded_update,
        ),
        candidate_frame=args.candidate_frame,
        candidate_frame_pin=Pin(args.candidate_frame_sha256, args.candidate_frame_bytes),
        builder_csv=args.builder_csv,
        builder_pin=Pin(args.builder_sha256, args.builder_bytes),
        expected_builder_rows=args.expected_builder_rows,
        expected_candidate_groups=args.expected_candidate_groups,
        output_csv=args.output_csv,
        receipt=args.receipt,
        status=args.status,
        overlay_mode=args.overlay_mode,
        minimum_free_bytes=args.minimum_free_bytes,
    )
    try:
        receipt = build(config)
    except BNetzASupplementError as exc:
        raise SystemExit(f"DE BNetzA supplement refused: {exc}") from exc
    print(json.dumps(receipt, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
