#!/usr/bin/env python3
"""Extract a provenance-bearing address corpus from Overture Places.

This is a deliberately manual, network-reading recipe.  It reads the pinned
Overture ``theme=places`` GeoParquet files through DuckDB, never the
``theme=addresses`` files used by some GridPin country builds.  The output is a
private runtime artifact under ``public-bench-work/`` by default; only this
recipe and its documentation belong in Git.

Selection is stable SHA-256 ranking, not storage order or ``first N``.  Every
row retains Overture coordinate-source provenance and receives a conservative
lineage class. Geometry-specific sources override root sources; unknown or
ambiguous source sets remain ``unknown_lineage``.
"""

from __future__ import annotations

import argparse
import collections
import contextlib
import datetime as dt
import fcntl
import hashlib
import io
import json
import math
import os
import pathlib
import platform
import re
import secrets
import shlex
import shutil
import stat
import sys
import unicodedata
from typing import BinaryIO, Iterable, Iterator, Mapping, Sequence


OVERTURE_RELEASE = "2026-06-17.0"
OVERTURE_THEME = "places"
OVERTURE_TYPE = "place"
OVERTURE_S3 = (
    "s3://overturemaps-us-west-2/release/"
    f"{OVERTURE_RELEASE}/theme={OVERTURE_THEME}/type={OVERTURE_TYPE}/*.parquet"
)
OVERTURE_SOURCE_URL = "https://docs.overturemaps.org/guides/places/"
CORPUS_SCHEMA = 3
LINEAGE_POLICY = "disclosed-public-coordinate-lineage-v1"
EXTRACTION_LINEAGE_POLICY = "overture-places-coordinate-source-v3"
DEFAULT_COUNTRIES = ("FR", "IT", "NL", "RS")
DEFAULT_PER_COUNTRY = 300
DEFAULT_FUTURE_PER_COUNTRY = 1_000
DEFAULT_MIN_UNKNOWN = 150
DEFAULT_CANDIDATE_MULTIPLIER = 24
DEFAULT_SEED = "gridpin-bl14-overture-places-v3"
GIB = 1024**3
UNKNOWN_LICENSE = "UNKNOWN"
PINNED_PYTHON_IMPLEMENTATION = "CPython"
PINNED_PYTHON_VERSION = (3, 11, 5)
PINNED_UNICODE_VERSION = "14.0.0"
PINNED_DUCKDB_VERSION = "1.5.3"
RAW_SCHEMA = 1
DIAGNOSTICS_SCHEMA = 1
SOURCE_COHORTS = ("outside_hint", "common_hint", "unknown_hint")
DIVERSITY_SHARD_COUNT = 256
FOURSQUARE_REVIEW_URL_PREFIX = "https://foursquare.com/placemakers/review-place/"
FOURSQUARE_RECORD_ID_RE = re.compile(r"[A-Za-z0-9_-]{1,128}\Z", re.ASCII)
REMOTE_CANDIDATE_HASH_FORMULA = (
    "COALESCE(CAST(id AS VARCHAR),'')|"
    "COALESCE(CAST(addresses[1].freeform AS VARCHAR),'')|"
    "COALESCE(CAST(bbox.ymin AS VARCHAR),'')|seed"
)
REMOTE_COHORT_FORMULA = (
    "effective geometry SourceItems, otherwise root SourceItems; exact-token "
    "Foursquare hints, country-ancestor hints, otherwise unknown hints"
)
REMOTE_DIVERSITY_SHARD_FORMULA = (
    "first two hex digits of SHA-256 over lower(locality)|lower(category)|"
    "lower(network)|latitude-rounded-2dp|longitude-rounded-2dp"
)
FINAL_SELECTION_HASH_FORMULA = (
    "seed|release|country|record_id|"
    "NFKC-casefold-alnum-space(query)|latitude:.7f|longitude:.7f"
)

COUNTRIES = {
    "FR": {"name": "France", "bounds": (-5.5, 41.0, 9.8, 51.5)},
    "IT": {"name": "Italy", "bounds": (6.6, 35.3, 18.6, 47.2)},
    "NL": {"name": "Netherlands", "bounds": (3.2, 50.7, 7.3, 53.7)},
    "RS": {"name": "Serbia", "bounds": (18.7, 41.8, 23.1, 46.3)},
}

# These are the exact ancestors recorded for the corresponding GridPin indexed
# country sheet, not generic open-data markers.  In particular, OSM is not
# enough to prove that a row shares the sheet's national address ancestor, and
# OpenAddresses is not a common ancestor for the French sheet.
#
# Each tuple is: canonical ancestor, phrase aliases, token aliases, evidence
# page. Short acronyms are token-matched so "BAN" cannot match "urban".
COUNTRY_COMMON_SOURCES = {
    "FR": (
        (
            "Base Adresse Nationale (BAN)",
            ("base adresse nationale",),
            ("ban",),
            "https://adresse.data.gouv.fr/donnees-nationales",
        ),
    ),
    "IT": (
        (
            "Archivio Nazionale dei Numeri Civici e delle Strade Urbane (ANNCSU)",
            (
                "archivio nazionale dei numeri civici e delle strade urbane",
                "archivio nazionale dei numeri civici",
            ),
            ("anncsu",),
            "https://www.anncsu.gov.it/",
        ),
        (
            "OpenAddresses",
            ("openaddresses", "open addresses"),
            (),
            "https://openaddresses.io/",
        ),
    ),
    "NL": (
        (
            "Basisregistratie Adressen en Gebouwen (BAG/Kadaster)",
            ("basisregistratie adressen en gebouwen", "kadaster"),
            ("bag",),
            "https://www.kadaster.nl/zakelijk/registraties/basisregistraties/bag",
        ),
        (
            "OpenAddresses",
            ("openaddresses", "open addresses"),
            (),
            "https://openaddresses.io/",
        ),
    ),
    "RS": (
        (
            "Republic Geodetic Authority (RGZ)",
            (
                "republic geodetic authority",
                "republicki geodetski zavod",
                "republički geodetski zavod",
            ),
            ("rgz",),
            "https://www.rgz.gov.rs/",
        ),
        (
            "OpenAddresses",
            ("openaddresses", "open addresses"),
            (),
            "https://openaddresses.io/",
        ),
    ),
}

OUTSIDE_PHRASES = ("foursquare",)
OUTSIDE_TOKENS = ("fsq",)
OSM_PHRASES = ("openstreetmap", "open street map")
OSM_TOKENS = ("osm",)
AMBIGUOUS_PHRASES = ("meta", "microsoft", "overture")


class CorpusError(RuntimeError):
    """A source, selection, or output contract could not be proved."""


def _utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )


def _canonical_utc(value: str) -> str:
    """Validate a seconds-precision UTC timestamp and return canonical ``Z`` form."""

    if not isinstance(value, str) or not re.fullmatch(
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:Z|\+00:00)", value
    ):
        raise CorpusError("retrieved_at must be seconds-precision ISO-8601 UTC")
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise CorpusError("retrieved_at must be a valid ISO-8601 UTC timestamp") from exc
    if parsed.utcoffset() != dt.timedelta(0):
        raise CorpusError("retrieved_at must use UTC")
    return parsed.strftime("%Y-%m-%dT%H:%M:%SZ")


def _runtime_identity(*, duckdb_version: str | None = None) -> dict[str, object]:
    identity: dict[str, object] = {
        "python_implementation": platform.python_implementation(),
        "python_version": platform.python_version(),
        "unicode_version": unicodedata.unidata_version,
    }
    if duckdb_version is not None:
        identity["duckdb_version"] = duckdb_version
    return identity


def _assert_runtime(*, duckdb_version: str | None = None) -> None:
    expected_python = ".".join(str(part) for part in PINNED_PYTHON_VERSION)
    if platform.python_implementation() != PINNED_PYTHON_IMPLEMENTATION:
        raise CorpusError(f"runtime must be {PINNED_PYTHON_IMPLEMENTATION}")
    if sys.version_info[:3] != PINNED_PYTHON_VERSION:
        raise CorpusError(
            f"runtime must be Python {expected_python}; got {platform.python_version()}"
        )
    if unicodedata.unidata_version != PINNED_UNICODE_VERSION:
        raise CorpusError(
            f"runtime must use Unicode {PINNED_UNICODE_VERSION}; "
            f"got {unicodedata.unidata_version}"
        )
    if duckdb_version is not None and duckdb_version != PINNED_DUCKDB_VERSION:
        raise CorpusError(
            f"runtime must use DuckDB {PINNED_DUCKDB_VERSION}; got {duckdb_version}"
        )


def _path_exists_nofollow(path: pathlib.Path) -> bool:
    try:
        path.lstat()
    except FileNotFoundError:
        return False
    return True


def _open_regular_nofollow(path: pathlib.Path, label: str) -> tuple[BinaryIO, os.stat_result]:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags)
    except OSError as exc:
        raise CorpusError(f"cannot open {label} without following symlinks: {path}: {exc}") from exc
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise CorpusError(f"{label} must be a single-link regular file: {path}")
        return os.fdopen(fd, "rb"), info
    except Exception:
        os.close(fd)
        raise


def _read_regular_bytes(path: pathlib.Path, label: str) -> tuple[bytes, os.stat_result]:
    handle, before = _open_regular_nofollow(path, label)
    with handle:
        payload = handle.read()
        after = os.fstat(handle.fileno())
    identity_before = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
    identity_after = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    if identity_before != identity_after:
        raise CorpusError(f"{label} changed while it was read: {path}")
    try:
        current = path.lstat()
    except FileNotFoundError as exc:
        raise CorpusError(f"{label} disappeared while it was read: {path}") from exc
    identity_path = (current.st_dev, current.st_ino, current.st_size, current.st_mtime_ns)
    if identity_path != identity_after or not stat.S_ISREG(current.st_mode):
        raise CorpusError(f"{label} path identity changed while it was read: {path}")
    return payload, after


def _script_capture() -> dict[str, object]:
    path = pathlib.Path(__file__).absolute()
    payload, info = _read_regular_bytes(path, "extractor script")
    return {
        "path": path,
        "sha256": hashlib.sha256(payload).hexdigest(),
        "identity": (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns),
    }


def _verify_script_capture(capture: Mapping[str, object]) -> None:
    path = pathlib.Path(str(capture["path"]))
    payload, info = _read_regular_bytes(path, "extractor script")
    identity = (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)
    if identity != capture["identity"] or hashlib.sha256(payload).hexdigest() != capture["sha256"]:
        raise CorpusError("extractor script changed after its start-of-run capture")


def _fsync_directory(path: pathlib.Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    fd = os.open(path, flags)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _atomic_bytes_noreplace(path: pathlib.Path, payload: bytes, label: str) -> None:
    """Publish bytes with an unpredictable same-directory temp and no replacement."""

    path = path.absolute()
    parent = path.parent
    parent.mkdir(parents=True, exist_ok=True)
    try:
        parent_info = parent.lstat()
    except OSError as exc:
        raise CorpusError(f"cannot inspect {label} parent: {parent}: {exc}") from exc
    if not stat.S_ISDIR(parent_info.st_mode) or stat.S_ISLNK(parent_info.st_mode):
        raise CorpusError(f"{label} parent must be a real directory: {parent}")
    if _path_exists_nofollow(path):
        raise CorpusError(f"refusing to replace existing {label}: {path}")
    temp = parent / f".{path.name}.{os.getpid()}.{secrets.token_hex(12)}.tmp"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd: int | None = None
    try:
        fd = os.open(temp, flags, 0o600)
        with os.fdopen(fd, "wb", closefd=True) as handle:
            fd = None
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        try:
            os.link(temp, path, follow_symlinks=False)
        except FileExistsError as exc:
            raise CorpusError(f"refusing to replace existing {label}: {path}") from exc
        published = path.lstat()
        if not stat.S_ISREG(published.st_mode) or published.st_nlink != 2:
            raise CorpusError(f"published {label} has unsafe filesystem identity: {path}")
        _fsync_directory(parent)
    finally:
        if fd is not None:
            os.close(fd)
        try:
            temp.unlink()
        except FileNotFoundError:
            pass


def _json_bytes(payload: Mapping[str, object]) -> bytes:
    return (
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    ).encode("utf-8")


def _jsonl_bytes(rows: Iterable[Mapping[str, object]]) -> bytes:
    output = io.StringIO()
    for row in rows:
        output.write(json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")))
        output.write("\n")
    return output.getvalue().encode("utf-8")


@contextlib.contextmanager
def _output_claim(path: pathlib.Path) -> Iterator[None]:
    """Serialize writers through a persistent, no-follow flock inode."""

    lock = path.absolute().with_suffix(path.suffix + ".lock")
    lock.parent.mkdir(parents=True, exist_ok=True)
    flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(lock, flags, 0o600)
    except OSError as exc:
        raise CorpusError(f"cannot open output lock safely: {lock}: {exc}") from exc
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise CorpusError(f"output lock must be a single-link regular file: {lock}")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise CorpusError(f"another extractor owns output lock: {lock}") from exc
        yield
    finally:
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)


def _sha256_file(path: pathlib.Path) -> str:
    payload, _ = _read_regular_bytes(path, "artifact")
    return hashlib.sha256(payload).hexdigest()


def _norm(value: object) -> str:
    folded = unicodedata.normalize("NFKC", str(value).casefold())
    return " ".join("".join(character if character.isalnum() else " " for character in folded).split())


def _tokens(value: str) -> set[str]:
    return set(_norm(value).split())


def _marker_match(value: str, phrases: Sequence[str], tokens: Sequence[str]) -> str | None:
    normalized = _norm(value)
    if not normalized:
        return None
    # Dataset/provider identities are trust-bearing.  Only a complete
    # normalized alias is accepted: ``NotFoursquare``, ``Not Foursquare`` and
    # ``FoursquareMirror`` must never inherit Foursquare's stronger class.
    return next(
        (
            marker
            for marker in (*phrases, *tokens)
            if normalized == _norm(marker)
        ),
        None,
    )


def _common_matches(
    source_datasets: Sequence[str], country: str
) -> list[tuple[str, str, str]]:
    """Return normalized identity, exact sheet ancestor, and evidence URL."""

    matches: set[tuple[str, str, str]] = set()
    for source_name in source_datasets:
        normalized = _norm(source_name)
        if not normalized:
            continue
        for ancestor, phrases, tokens, evidence_url in COUNTRY_COMMON_SOURCES[country]:
            if _marker_match(normalized, phrases, tokens):
                matches.add((normalized, ancestor, evidence_url))
    return sorted(matches)


def _declared_license(source: Mapping[str, object]) -> str:
    return _source_text(source, "license")


def _license_is_unambiguous(value: str) -> bool:
    """Accept an exact disclosed license string, never an inferred fallback."""

    normalized = _norm(value)
    if not normalized:
        return False
    words = _tokens(normalized)
    if words & {"unknown", "unspecified", "various", "multiple", "verify"}:
        return False
    if "or" in words or "source specific" in normalized or "see attribution" in normalized:
        return False
    return True


def _license_proof_failures(
    sources: Sequence[Mapping[str, object]],
) -> list[str]:
    failures: list[str] = []
    for number, source in enumerate(sources, 1):
        declared = _declared_license(source)
        if not declared:
            failures.append(f"coordinate source #{number} has no declared license")
        elif not _license_is_unambiguous(declared):
            failures.append(
                f"coordinate source #{number} has ambiguous declared license: {declared}"
            )
    return failures


def _valid_foursquare_record_id(value: str) -> bool:
    return FOURSQUARE_RECORD_ID_RE.fullmatch(value) is not None


def _decode_sources(raw: object) -> list[dict[str, object]]:
    if isinstance(raw, str):
        try:
            raw = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise CorpusError(f"invalid Overture sources JSON: {exc}") from exc
    if raw is None:
        return []
    if not isinstance(raw, list):
        raise CorpusError("Overture sources must be a list")
    decoded: list[dict[str, object]] = []
    for number, source in enumerate(raw, 1):
        if not isinstance(source, Mapping):
            raise CorpusError(f"Overture source #{number} is not an object")
        try:
            preserved = json.loads(
                json.dumps(dict(source), ensure_ascii=False, allow_nan=False)
            )
        except (TypeError, ValueError) as exc:
            raise CorpusError(f"Overture source #{number} is not JSON-serializable") from exc
        if not isinstance(preserved, dict):
            raise CorpusError(f"Overture source #{number} did not preserve as an object")
        decoded.append(preserved)
    return decoded


def _source_text(source: Mapping[str, object], field: str) -> str:
    value = source.get(field)
    if value is None:
        return ""
    if not isinstance(value, str):
        raise CorpusError(f"Overture SourceItem field {field!r} must be a string")
    return value.strip()


def _source_identities(source: Mapping[str, object]) -> list[str]:
    return sorted({
        value
        for value in (
            _source_text(source, "dataset"),
            _source_text(source, "provider"),
        )
        if value
    })


def _coordinate_sources(
    sources: Sequence[dict[str, object]],
) -> tuple[list[dict[str, object]], str]:
    geometry = [
        source for source in sources
        if _source_text(source, "property").casefold().rstrip("/") == "/geometry"
    ]
    if geometry:
        return geometry, "geometry_override"
    roots = [source for source in sources if not _source_text(source, "property")]
    return roots, "root_fallback"


def classify_coordinate_sources(
    sources: Sequence[dict[str, object]], country: str
) -> tuple[
    str,
    list[str],
    list[dict[str, object]],
    str,
    str | None,
    str | None,
]:
    scoped, scope = _coordinate_sources(sources)
    identities = [identity for source in scoped for identity in _source_identities(source)]
    lineage_class, evidence = classify_lineage(identities, country)
    license_failures = _license_proof_failures(scoped)
    if lineage_class == "common_upstream":
        if license_failures:
            return (
                "unknown_lineage",
                sorted([*evidence, *license_failures]),
                scoped,
                scope,
                None,
                None,
            )
        common_matches = _common_matches(identities, country)
        ancestors = sorted({match[1] for match in common_matches})
        evidence_urls = sorted({match[2] for match in common_matches})
        if len(ancestors) != 1 or len(evidence_urls) != 1:
            return (
                "unknown_lineage",
                sorted(["multiple exact common ancestors disclosed", *evidence]),
                scoped,
                scope,
                None,
                None,
            )
        return (
            lineage_class,
            evidence,
            scoped,
            scope,
            ancestors[0],
            evidence_urls[0],
        )
    if len(scoped) != 1:
        detail = f"{len(scoped)} coordinate source records disclosed"
        return "unknown_lineage", sorted([detail, *evidence]), scoped, scope, None, None
    if lineage_class == "outside_chain":
        source_record_id = _source_text(scoped[0], "record_id")
        if not source_record_id:
            return (
                "unknown_lineage",
                sorted(["outside-chain coordinate source has no upstream record id", *evidence]),
                scoped,
                scope,
                None,
                None,
            )
        if not _valid_foursquare_record_id(source_record_id):
            return (
                "unknown_lineage",
                sorted(
                    ["Foursquare coordinate source has an unsafe record id", *evidence]
                ),
                scoped,
                scope,
                None,
                None,
            )
        if license_failures:
            return (
                "unknown_lineage",
                sorted([*evidence, *license_failures]),
                scoped,
                scope,
                None,
                None,
            )
        return (
            lineage_class,
            evidence,
            scoped,
            scope,
            None,
            FOURSQUARE_REVIEW_URL_PREFIX + source_record_id,
        )
    return lineage_class, evidence, scoped, scope, None, None


def classify_lineage(source_datasets: Sequence[str], country: str) -> tuple[str, list[str]]:
    """Classify source identities against country-specific sheet ancestry.

    This helper screens names only. ``classify_coordinate_sources`` additionally
    requires a declared license and, for Foursquare, a safe upstream record id.
    """

    if country not in COUNTRIES:
        raise CorpusError(f"unsupported country {country!r}")
    names = [_norm(item) for item in source_datasets if _norm(item)]
    if not names:
        return "unknown_lineage", ["no direct source dataset disclosed"]

    osm_evidence = [
        f"{name} -> OSM is not a proved national-sheet ancestor"
        for name in names
        if _marker_match(name, OSM_PHRASES, OSM_TOKENS)
    ]
    if osm_evidence:
        return "unknown_lineage", sorted(osm_evidence)

    common_matches = _common_matches(names, country)
    ancestors = sorted({match[1] for match in common_matches})
    common_evidence = [f"{name} -> {ancestor}" for name, ancestor, _ in common_matches]
    if len(ancestors) == 1:
        return "common_upstream", sorted(common_evidence)
    if len(ancestors) > 1:
        return "unknown_lineage", sorted(
            ["multiple exact common ancestors disclosed", *common_evidence]
        )

    outside: list[str] = []
    ambiguous: list[str] = []
    for name in names:
        marker = _marker_match(name, OUTSIDE_PHRASES, OUTSIDE_TOKENS)
        if marker:
            outside.append(f"{name} -> {marker}")
        else:
            ambiguity = _marker_match(name, AMBIGUOUS_PHRASES, ())
            ambiguous.append(f"{name} -> {ambiguity or 'unrecognized source'}")
    if outside and not ambiguous:
        return "outside_chain", sorted(outside)
    return "unknown_lineage", sorted(outside + ambiguous)


def coordinate_licenses(sources: Sequence[Mapping[str, object]]) -> list[str]:
    """Return only disclosed license strings, making every gap explicit."""

    licenses = {_declared_license(source) or UNKNOWN_LICENSE for source in sources}
    return sorted(licenses or {UNKNOWN_LICENSE})


def _query_parts(address: Mapping[str, str], country_name: str) -> str:
    locality = " ".join(
        part for part in (address["postcode"], address["locality"]) if part
    )
    return ", ".join(
        part for part in (address["freeform"], locality, country_name) if part
    )


def _finite_in_bounds(country: str, lat: float, lon: float) -> bool:
    west, south, east, north = COUNTRIES[country]["bounds"]
    return math.isfinite(lat) and math.isfinite(lon) and south <= lat <= north and west <= lon <= east


def _selection_material(row: Mapping[str, object], seed: str) -> str:
    return "|".join(
        (
            seed,
            OVERTURE_RELEASE,
            str(row["country"]),
            str(row["record_id"]),
            _norm(row["query"]),
            f"{float(row['lat']):.7f}",
            f"{float(row['lon']):.7f}",
        )
    )


def _selection_hash(row: Mapping[str, object], seed: str) -> str:
    return hashlib.sha256(_selection_material(row, seed).encode("utf-8")).hexdigest()


def candidate_from_raw(raw: Mapping[str, object], retrieved_at: str, seed: str) -> dict[str, object]:
    retrieved_at = _canonical_utc(retrieved_at)
    country = str(raw.get("country", "")).upper()
    if country not in COUNTRIES:
        raise CorpusError(f"unsupported candidate country {country!r}")
    record_id = str(raw.get("record_id", "")).strip()
    if not record_id:
        raise CorpusError("candidate has no Overture record id")
    try:
        lat = float(raw["lat"])
        lon = float(raw["lon"])
        confidence = float(raw["confidence"])
    except (KeyError, TypeError, ValueError) as exc:
        raise CorpusError(f"{record_id}: invalid coordinate or confidence") from exc
    if not _finite_in_bounds(country, lat, lon):
        raise CorpusError(f"{record_id}: non-finite or out-of-bounds coordinate")
    if not math.isfinite(confidence) or not 0.0 <= confidence <= 1.0:
        raise CorpusError(f"{record_id}: confidence is not finite in [0, 1]")

    address = {
        key: " ".join(str(raw.get(key, "") or "").split())
        for key in ("freeform", "postcode", "locality", "region")
    }
    address["country"] = country
    if not any(character.isalpha() for character in address["freeform"]) or not any(
        character.isdigit() for character in address["freeform"]
    ):
        raise CorpusError(f"{record_id}: street address needs alphabetic text and a number")
    if address["postcode"] and not any(
        character.isdigit() for character in address["postcode"]
    ):
        raise CorpusError(f"{record_id}: non-empty postcode has no digit")
    if not any(character.isalpha() for character in address["locality"]):
        raise CorpusError(f"{record_id}: municipality has no alphabetic text")
    query = _query_parts(address, str(COUNTRIES[country]["name"]))

    sources = _decode_sources(raw.get("sources"))
    root_sources = [source for source in sources if not _source_text(source, "property")]
    (
        lineage_class,
        lineage_evidence,
        coordinate_sources,
        coordinate_scope,
        common_ancestor,
        evidence_url,
    ) = classify_coordinate_sources(sources, country)
    all_datasets = sorted(
        {_source_text(source, "dataset") for source in sources if _source_text(source, "dataset")}
    )
    root_datasets = sorted(
        {
            _source_text(source, "dataset")
            for source in root_sources
            if _source_text(source, "dataset")
        }
    )
    coordinate_datasets = sorted(
        {
            _source_text(source, "dataset")
            for source in coordinate_sources
            if _source_text(source, "dataset")
        }
    )
    licenses = coordinate_licenses(coordinate_sources)
    license_text = " AND ".join(licenses)
    coordinate_identities = sorted({
        identity for source in coordinate_sources for identity in _source_identities(source)
    })
    coordinate_record_ids = sorted({
        _source_text(source, "record_id")
        for source in coordinate_sources
        if _source_text(source, "record_id")
    })
    complete_coordinate_ids = bool(coordinate_sources) and all(
        _source_text(source, "record_id") for source in coordinate_sources
    )
    coordinate_provenance = {
        "source_name": (
            " + ".join(coordinate_identities)
            if complete_coordinate_ids and coordinate_identities
            else "Overture Maps Places"
        ),
        "source_url": OVERTURE_SOURCE_URL,
        "record_id": " + ".join(coordinate_record_ids) if complete_coordinate_ids else record_id,
        "retrieved_at": retrieved_at,
        "license": license_text,
        "common_ancestor": common_ancestor,
        "evidence_url": evidence_url,
        "same_export_as_indexed_sheet": False,
    }
    category = " ".join(str(raw.get("category", "") or "").split())
    row: dict[str, object] = {
        "schema": CORPUS_SCHEMA,
        "country": country,
        "query": query,
        "street_address": address["freeform"],
        "postcode": address["postcode"],
        "municipality": address["locality"],
        "address": address,
        "lat": lat,
        "lon": lon,
        "network": " ".join(str(raw.get("network", "") or "").split()),
        "confidence": confidence,
        "record_id": record_id,
        "coordinate_source_dataset": coordinate_datasets,
        "coordinate_source_scope": coordinate_scope,
        "coordinate_source_records": coordinate_sources,
        "root_source_dataset": root_datasets,
        "root_source_records": root_sources,
        "source_dataset": coordinate_datasets,
        "all_source_datasets": all_datasets,
        "source_records": sources,
        "source_release": OVERTURE_RELEASE,
        "source_theme": OVERTURE_THEME,
        "source_url": OVERTURE_SOURCE_URL,
        "coordinate_provenance": coordinate_provenance,
        "license": license_text,
        "licenses": licenses,
        "retrieved_at": retrieved_at,
        "lineage_class": lineage_class,
        "lineage_evidence": lineage_evidence,
        "lineage_policy": EXTRACTION_LINEAGE_POLICY,
    }
    source_cohort = raw.get("source_cohort")
    if source_cohort is not None:
        if not isinstance(source_cohort, str) or source_cohort not in SOURCE_COHORTS:
            raise CorpusError(f"{record_id}: invalid acquisition source cohort")
        row["acquisition_source_cohort"] = source_cohort
    if category:
        row["category"] = category
    row["selection_sha256"] = _selection_hash(row, seed)
    return row


def automatic_caps(per_country: int) -> dict[str, int]:
    return {
        "city": max(8, math.ceil(per_country * 0.04)),
        "category": max(15, math.ceil(per_country * 0.08)),
        "network": max(6, math.ceil(per_country * 0.03)),
    }


def _fits_caps(
    row: Mapping[str, object], counts: Mapping[str, collections.Counter[str]], caps: Mapping[str, int]
) -> bool:
    keys = {
        "city": _norm(row["address"]["locality"]),
        "category": _norm(row.get("category", "")) or "(uncategorized)",
        "network": _norm(row["network"]),
    }
    return all(
        not key or counts[dimension][key] < caps[dimension]
        for dimension, key in keys.items()
    )


def _add_counts(
    row: Mapping[str, object], counts: Mapping[str, collections.Counter[str]]
) -> None:
    keys = {
        "city": _norm(row["address"]["locality"]),
        "category": _norm(row.get("category", "")) or "(uncategorized)",
        "network": _norm(row["network"]),
    }
    for dimension, key in keys.items():
        if key:
            counts[dimension][key] += 1


def _diversity_keys(row: Mapping[str, object]) -> dict[str, str]:
    return {
        "city": _norm(row["address"]["locality"]),
        "category": _norm(row.get("category", "")) or "(uncategorized)",
        "network": _norm(row["network"]),
    }


def _constraint_aware_order(
    rows: Iterable[dict[str, object]], caps: Mapping[str, int]
) -> list[dict[str, object]]:
    """Prefer scarce diversity groups, retaining the stable hash as tie-breaker."""

    materialized = list(rows)
    frequencies = {name: collections.Counter() for name in caps}
    for row in materialized:
        for dimension, key in _diversity_keys(row).items():
            if key:
                frequencies[dimension][key] += 1

    def score(row: Mapping[str, object]) -> tuple[float, float, str, str]:
        pressures = [
            frequencies[dimension][key] / caps[dimension]
            for dimension, key in _diversity_keys(row).items()
            if key
        ]
        return (
            max(pressures, default=0.0),
            sum(pressures),
            str(row["selection_sha256"]),
            str(row["record_id"]),
        )

    return sorted(materialized, key=score)


def _dedupe_key(row: Mapping[str, object]) -> tuple[str, str, tuple[float, float]]:
    return (
        str(row["record_id"]),
        "".join(
            character.casefold()
            for character in str(row["query"])
            if character.isalnum()
        ),
        (round(float(row["lat"]), 5), round(float(row["lon"]), 5)),
    )


def _pool_capacity_upper_bound(
    rows: Iterable[Mapping[str, object]], caps: Mapping[str, int]
) -> dict[str, object]:
    """Return deterministic necessary upper bounds for any valid selection.

    Each deduplication key and each capped diversity group is an independent
    packing constraint.  The minimum of their capacities is therefore a safe
    upper bound on the number of rows any selector (heuristic or exact) could
    retain.  This is deliberately only a one-way certificate: meeting the
    bound does not prove feasibility, while falling below a required quota is
    a mathematical proof of infeasibility for the saved snapshot.
    """

    materialized = list(rows)
    component_bounds: dict[str, int] = {
        "candidate_rows": len(materialized),
        "distinct_record_ids": len({_dedupe_key(row)[0] for row in materialized}),
        "distinct_runner_address_keys": len(
            {_dedupe_key(row)[1] for row in materialized}
        ),
        "distinct_coordinates_rounded_5dp": len(
            {_dedupe_key(row)[2] for row in materialized}
        ),
    }
    for dimension, cap in sorted(caps.items()):
        frequencies = collections.Counter(
            _diversity_keys(row)[dimension] for row in materialized
        )
        # Empty keys are not counted by _fits_caps/_add_counts and therefore
        # remain uncapped.  Category already uses the explicit
        # ``(uncategorized)`` key and is capped like every other category.
        component_bounds[f"{dimension}_cap_capacity"] = sum(
            count if not key else min(count, cap)
            for key, count in frequencies.items()
        )
    upper_bound = min(component_bounds.values(), default=0)
    limiting = sorted(
        name for name, value in component_bounds.items() if value == upper_bound
    )
    return {
        "upper_bound": upper_bound,
        "limiting_constraints": limiting,
        "component_upper_bounds": dict(sorted(component_bounds.items())),
    }


def _selection_feasibility_certificate(
    rows: Sequence[Mapping[str, object]],
    per_country: int,
    min_unknown: int,
    caps: Mapping[str, int],
) -> dict[str, object]:
    """Prove impossible quotas when elementary packing bounds are decisive."""

    all_rows = _pool_capacity_upper_bound(rows, caps)
    unknown_rows = _pool_capacity_upper_bound(
        (row for row in rows if row["lineage_class"] == "unknown_lineage"),
        caps,
    )
    proofs: list[dict[str, object]] = []
    for pool_name, required, bound in (
        ("all_rows", per_country, all_rows),
        ("unknown_lineage", min_unknown, unknown_rows),
    ):
        upper_bound = int(bound["upper_bound"])
        if upper_bound < required:
            proofs.append(
                {
                    "pool": pool_name,
                    "required": required,
                    "upper_bound": upper_bound,
                    "limiting_constraints": list(bound["limiting_constraints"]),
                }
            )
    return {
        "kind": "necessary_packing_upper_bounds",
        "scope": "verified offline acquisition snapshot",
        "all_rows": all_rows,
        "unknown_lineage": unknown_rows,
        "proves_infeasible": bool(proofs),
        "proofs": proofs,
    }


def _select_country_attempt(
    rows: Iterable[dict[str, object]], per_country: int, min_unknown: int, caps: Mapping[str, int]
) -> tuple[list[dict[str, object]], str | None]:
    ordered = sorted(rows, key=lambda row: (str(row["selection_sha256"]), str(row["record_id"])))
    counts = {name: collections.Counter() for name in caps}
    selected: list[dict[str, object]] = []
    selected_ids: set[str] = set()
    seen_ids: set[str] = set()
    seen_queries: set[str] = set()
    seen_coordinates: set[tuple[float, float]] = set()

    def add(pool: Iterable[dict[str, object]], target: int) -> None:
        for row in pool:
            if len(selected) >= target:
                return
            record_id, query, coordinate = _dedupe_key(row)
            if record_id in seen_ids or query in seen_queries or coordinate in seen_coordinates:
                continue
            if not _fits_caps(row, counts, caps):
                continue
            seen_ids.add(record_id)
            seen_queries.add(query)
            seen_coordinates.add(coordinate)
            selected.append(row)
            selected_ids.add(record_id)
            _add_counts(row, counts)

    unknown = _constraint_aware_order(
        (row for row in ordered if row["lineage_class"] == "unknown_lineage"), caps
    )
    add(unknown, min_unknown)
    if len(selected) < min_unknown:
        return (
            sorted(selected, key=lambda row: str(row["selection_sha256"])),
            f"only {len(selected)} diverse unknown-lineage rows survived; "
            f"{min_unknown} required",
        )
    add(
        _constraint_aware_order(
            (row for row in ordered if str(row["record_id"]) not in selected_ids), caps
        ),
        per_country,
    )
    if len(selected) < per_country:
        return (
            sorted(selected, key=lambda row: str(row["selection_sha256"])),
            f"only {len(selected)} diverse rows survived; {per_country} required",
        )
    return sorted(selected, key=lambda row: str(row["selection_sha256"])), None


def select_country(
    rows: Iterable[dict[str, object]], per_country: int, min_unknown: int, caps: Mapping[str, int]
) -> list[dict[str, object]]:
    selected, error = _select_country_attempt(rows, per_country, min_unknown, caps)
    if error is not None:
        raise CorpusError(error)
    return selected


def _sql_string(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def candidate_query(
    countries: Sequence[str] | str, candidate_limit: int, seed: str
) -> str:
    """Build one spatially-pruned, bounded candidate query.

    Overture ``type=place`` geometries are Points.  Their GeoParquet bbox has
    identical min/max coordinates, which lets the hot remote scan avoid WKB
    decoding and the spatial extension.  Each country code remains coupled to
    its own bbox so the combined scan cannot admit a row through another
    country's wider bounds.
    """

    requested = (countries,) if isinstance(countries, str) else tuple(countries)
    if not requested:
        raise CorpusError("at least one country is required for the candidate query")
    unsupported = sorted(set(requested) - set(COUNTRIES))
    if unsupported:
        raise CorpusError(f"unsupported countries: {', '.join(unsupported)}")
    if candidate_limit <= 0:
        raise CorpusError("candidate limit must be positive")
    per_shard_limit = math.ceil(candidate_limit / DIVERSITY_SHARD_COUNT)
    country_predicates = []
    for country in requested:
        west, south, east, north = COUNTRIES[country]["bounds"]
        country_predicates.append(
            "("
            f"upper(cast(p.addresses[1].country AS VARCHAR)) = {_sql_string(country)} "
            f"AND p.bbox.xmin <= {east} AND p.bbox.xmax >= {west} "
            f"AND p.bbox.ymin <= {north} AND p.bbox.ymax >= {south}"
            ")"
        )
    spatial_country_filter = "\n          OR ".join(country_predicates)
    stable_material = (
        "coalesce(cast(p.id as varchar), '') || '|' || "
        "coalesce(cast(p.addresses[1].freeform as varchar), '') || '|' || "
        "coalesce(cast(p.bbox.ymin as varchar), '') || '|' || "
        f"{_sql_string(seed)}"
    )
    return f"""
WITH eligible_base AS (
    SELECT upper(cast(p.addresses[1].country AS VARCHAR)) AS country,
           cast(p.id AS VARCHAR) AS record_id,
           trim(cast(p.addresses[1].freeform AS VARCHAR)) AS freeform,
           coalesce(cast(p.addresses[1].postcode AS VARCHAR), '') AS postcode,
           coalesce(cast(p.addresses[1].locality AS VARCHAR), '') AS locality,
           coalesce(cast(p.addresses[1].region AS VARCHAR), '') AS region,
           coalesce(cast(p.categories.primary AS VARCHAR), '') AS category,
           coalesce(cast(p.brand.names.primary AS VARCHAR), '') AS network,
           cast(p.confidence AS DOUBLE) AS confidence,
           cast(p.bbox.ymin AS DOUBLE) AS lat,
           cast(p.bbox.xmin AS DOUBLE) AS lon,
           p.sources AS sources,
           sha256({stable_material}) AS source_order_sha256
    FROM read_parquet({_sql_string(OVERTURE_S3)}, hive_partitioning=true) AS p
    WHERE ({spatial_country_filter})
      AND p.addresses[1].freeform IS NOT NULL
      AND regexp_matches(cast(p.addresses[1].freeform AS VARCHAR), '[0-9]')
      AND nullif(trim(cast(p.addresses[1].locality AS VARCHAR)), '') IS NOT NULL
      AND p.id IS NOT NULL
      AND p.bbox.xmin = p.bbox.xmax
      AND p.bbox.ymin = p.bbox.ymax
      AND p.confidence IS NOT NULL
), scoped AS (
    SELECT *,
           CASE
             WHEN coalesce(len(list_filter(
                    sources,
                    s -> lower(trim(coalesce(cast(s.property AS VARCHAR), ''))) = '/geometry'
                  )), 0) > 0
             THEN list_filter(
                    sources,
                    s -> lower(trim(coalesce(cast(s.property AS VARCHAR), ''))) = '/geometry'
                  )
             ELSE list_filter(
                    sources,
                    s -> trim(coalesce(cast(s.property AS VARCHAR), '')) = ''
                  )
           END AS coordinate_sources
    FROM eligible_base
), eligible AS (
    SELECT *,
           CASE
             WHEN coalesce(len(coordinate_sources), 0) > 0
              AND len(list_filter(
                    coordinate_sources,
                    s -> lower(trim(coalesce(cast(s.dataset AS VARCHAR), '')))
                         IN ('foursquare', 'fsq')
                  )) = len(coordinate_sources)
               THEN 'outside_hint'
             WHEN coalesce(len(list_filter(
                    coordinate_sources,
                    s -> lower(trim(coalesce(cast(s.dataset AS VARCHAR), '')))
                         IN (
                           'base adresse nationale', 'ban', 'anncsu',
                           'openaddresses', 'open addresses',
                           'basisregistratie adressen en gebouwen', 'kadaster', 'bag',
                           'republic geodetic authority', 'republicki geodetski zavod',
                           'republički geodetski zavod', 'rgz'
                         )
                  )), 0) > 0
               THEN 'common_hint'
             ELSE 'unknown_hint'
           END AS source_cohort,
           substr(
             sha256(
               lower(locality) || '|' || lower(category) || '|' || lower(network) || '|' ||
               cast(round(lat, 2) AS VARCHAR) || '|' || cast(round(lon, 2) AS VARCHAR)
             ),
             1,
             2
           ) AS diversity_shard
    FROM scoped
), pooled AS (
    SELECT country, source_cohort, diversity_shard,
           min_by(
               struct_pack(
                   country := country,
                   source_cohort := source_cohort,
                   diversity_shard := diversity_shard,
                   record_id := record_id,
                   freeform := freeform,
                   postcode := postcode,
                   locality := locality,
                   region := region,
                   category := category,
                   network := network,
                   confidence := confidence,
                   lat := lat,
                   lon := lon,
                   sources := sources,
                   source_order_sha256 := source_order_sha256
               ),
               source_order_sha256 || '|' || record_id,
               {int(per_shard_limit)}
           ) AS candidates
    FROM eligible
    GROUP BY country, source_cohort, diversity_shard
)
SELECT c.country, c.source_cohort, c.diversity_shard, c.source_order_sha256,
       c.record_id, c.freeform, c.postcode, c.locality, c.region,
       c.category, c.network, c.confidence, c.lat, c.lon,
       cast(to_json(c.sources) AS VARCHAR) AS sources
FROM pooled, unnest(candidates) AS retained(c)
ORDER BY c.country, c.source_cohort, c.diversity_shard,
         c.source_order_sha256, c.record_id
""".strip()


def _load_candidates(
    args: argparse.Namespace,
) -> tuple[list[dict[str, object]], str, str]:
    try:
        import duckdb
    except ImportError as exc:
        raise CorpusError("DuckDB is required; install the pinned version from the recipe") from exc

    args.output.parent.mkdir(parents=True, exist_ok=True)
    def check_disk(label: str) -> None:
        free = shutil.disk_usage(args.output.parent).free
        if free < 5 * GIB:
            raise CorpusError(
                f"disk guard before {label}: only {free / GIB:.1f} GiB free; "
                "at least 5 GiB required"
            )
        if free < 10 * GIB:
            print(
                f"WARNING: {free / GIB:.1f} GiB free. Do not start a second heavy service.",
                file=sys.stderr,
            )
        print(f"disk guard before {label}: {free / GIB:.1f} GiB free", file=sys.stderr)

    check_disk("DuckDB extension downloads")
    _assert_runtime(duckdb_version=str(duckdb.__version__))
    connection = duckdb.connect()
    connection.execute("SET threads=2")
    connection.execute("SET memory_limit='2GB'")
    connection.execute("SET max_temp_directory_size='2GB'")
    connection.execute("INSTALL httpfs; LOAD httpfs")
    connection.execute("SET s3_region='us-west-2'")
    connection.execute("SET http_timeout=120000")
    connection.execute("SET http_retries=8")
    candidate_limit = args.per_country * args.candidate_multiplier
    check_disk(f"combined {','.join(args.countries)} S3 read")
    query = candidate_query(args.countries, candidate_limit, args.seed)
    cursor = connection.execute(query)
    columns = [description[0] for description in cursor.description]
    raw_rows: list[dict[str, object]] = []
    while True:
        batch = cursor.fetchmany(2_000)
        if not batch:
            break
        raw_rows.extend(dict(zip(columns, values)) for values in batch)
    connection.close()
    return raw_rows, str(duckdb.__version__), query


def _write_jsonl(path: pathlib.Path, rows: Sequence[Mapping[str, object]]) -> None:
    _atomic_bytes_noreplace(path, _jsonl_bytes(rows), "JSONL artifact")


def _write_json(path: pathlib.Path, payload: Mapping[str, object], label: str) -> None:
    _atomic_bytes_noreplace(path, _json_bytes(payload), label)


def _logical_command(argv: Sequence[str]) -> str:
    logical: list[str] = ["python3", "examples/overture_places_corpus.py"]
    for value in argv:
        if "/" in value or "\\" in value:
            logical.append(pathlib.Path(value).name)
        else:
            logical.append(value)
    return shlex.join(logical)


def _raw_manifest_path(raw_path: pathlib.Path) -> pathlib.Path:
    return raw_path.with_suffix(raw_path.suffix + ".manifest.json")


def _raw_counts(rows: Sequence[Mapping[str, object]]) -> dict[str, object]:
    by_country = collections.Counter(str(row.get("country", "")) for row in rows)
    by_country_and_cohort: dict[str, dict[str, int]] = {}
    for country in sorted(by_country):
        cohort = collections.Counter(
            str(row.get("source_cohort", ""))
            for row in rows
            if str(row.get("country", "")) == country
        )
        by_country_and_cohort[country] = dict(sorted(cohort.items()))
    by_country_cohort_shard = collections.Counter(
        f"{row.get('country', '')}:{row.get('source_cohort', '')}:{row.get('diversity_shard', '')}"
        for row in rows
    )
    return {
        "rows_by_country": dict(sorted(by_country.items())),
        "rows_by_country_and_source_cohort": by_country_and_cohort,
        "rows_by_country_source_cohort_and_diversity_shard": dict(
            sorted(by_country_cohort_shard.items())
        ),
    }


def _acquisition_manifest(
    args: argparse.Namespace,
    raw_rows: Sequence[Mapping[str, object]],
    raw_path: pathlib.Path,
    raw_sha256: str,
    duckdb_version: str,
    query: str,
    script_capture: Mapping[str, object],
) -> dict[str, object]:
    counts = _raw_counts(raw_rows)
    return {
        "schema": RAW_SCHEMA,
        "kind": "overture_places_raw_candidate_acquisition",
        "status": "complete",
        "artifact": {
            "path": raw_path.name,
            "sha256": raw_sha256,
            "rows": len(raw_rows),
        },
        **counts,
        "source_details": {
            "dataset": "Overture Maps Foundation",
            "theme": OVERTURE_THEME,
            "type": OVERTURE_TYPE,
            "source_release": OVERTURE_RELEASE,
            "uri": OVERTURE_S3,
            "retrieved_at": args.retrieved_at,
        },
        "acquisition": {
            "countries": list(args.countries),
            "seed": args.seed,
            "per_country_target_at_acquisition": args.per_country,
            "candidate_multiplier_per_source_cohort": args.candidate_multiplier,
            "candidate_limit_per_country_and_source_cohort": (
                args.per_country * args.candidate_multiplier
            ),
            "candidate_limit_per_diversity_shard": math.ceil(
                args.per_country * args.candidate_multiplier / DIVERSITY_SHARD_COUNT
            ),
            "source_cohorts": list(SOURCE_COHORTS),
            "source_cohort_formula": REMOTE_COHORT_FORMULA,
            "diversity_shards": DIVERSITY_SHARD_COUNT,
            "diversity_shard_formula": REMOTE_DIVERSITY_SHARD_FORMULA,
            "remote_hash_formula": REMOTE_CANDIDATE_HASH_FORMULA,
            "query_sha256": hashlib.sha256(query.encode("utf-8")).hexdigest(),
        },
        "runtime": _runtime_identity(duckdb_version=duckdb_version),
        "recipe": {
            "script": "examples/overture_places_corpus.py",
            "script_start_sha256": str(script_capture["sha256"]),
            "command": args.logical_command,
        },
    }


def _read_json_mapping(path: pathlib.Path, label: str) -> tuple[dict[str, object], str]:
    payload, _ = _read_regular_bytes(path, label)
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise CorpusError(f"invalid {label}: {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise CorpusError(f"{label} must contain a JSON object: {path}")
    return value, hashlib.sha256(payload).hexdigest()


def _read_raw_snapshot(
    args: argparse.Namespace,
) -> tuple[list[dict[str, object]], str, dict[str, object], str]:
    raw_path = args.from_acquisition
    manifest_path = _raw_manifest_path(raw_path)
    manifest, manifest_sha256 = _read_json_mapping(
        manifest_path, "acquisition manifest"
    )
    if manifest.get("schema") != RAW_SCHEMA or manifest.get("status") != "complete":
        raise CorpusError("acquisition manifest is not a complete supported snapshot")
    artifact = manifest.get("artifact")
    source = manifest.get("source_details")
    acquisition = manifest.get("acquisition")
    runtime = manifest.get("runtime")
    if not all(isinstance(value, dict) for value in (artifact, source, acquisition, runtime)):
        raise CorpusError("acquisition manifest is missing required objects")
    assert isinstance(artifact, dict) and isinstance(source, dict)
    assert isinstance(acquisition, dict) and isinstance(runtime, dict)
    if artifact.get("path") != raw_path.name:
        raise CorpusError("acquisition artifact path is not relative or does not match")
    if source.get("source_release") != OVERTURE_RELEASE or source.get("theme") != OVERTURE_THEME:
        raise CorpusError("acquisition source release/theme does not match the pinned recipe")
    if source.get("type") != OVERTURE_TYPE or source.get("uri") != OVERTURE_S3:
        raise CorpusError("acquisition source type/URI does not match the pinned recipe")
    retrieved_at = _canonical_utc(str(source.get("retrieved_at", "")))
    if list(acquisition.get("countries", [])) != list(args.countries):
        raise CorpusError("acquisition countries do not match requested countries")
    if acquisition.get("seed") != args.seed:
        raise CorpusError("acquisition seed does not match requested seed")
    if acquisition.get("per_country_target_at_acquisition") != args.per_country:
        raise CorpusError("acquisition per-country target does not match requested target")
    expected_limit = args.per_country * args.candidate_multiplier
    if acquisition.get("candidate_limit_per_country_and_source_cohort") != expected_limit:
        raise CorpusError("acquisition candidate limit does not match requested selection")
    if acquisition.get("candidate_limit_per_diversity_shard") != math.ceil(
        expected_limit / DIVERSITY_SHARD_COUNT
    ):
        raise CorpusError("acquisition per-shard limit does not match the pinned query")
    if acquisition.get("diversity_shards") != DIVERSITY_SHARD_COUNT:
        raise CorpusError("acquisition diversity shard count does not match the pinned query")
    if acquisition.get("diversity_shard_formula") != REMOTE_DIVERSITY_SHARD_FORMULA:
        raise CorpusError("acquisition diversity shard formula does not match")
    expected_query = candidate_query(args.countries, expected_limit, args.seed)
    if acquisition.get("query_sha256") != hashlib.sha256(expected_query.encode("utf-8")).hexdigest():
        raise CorpusError("acquisition query hash does not match the current pinned query")
    raw_payload, _ = _read_regular_bytes(raw_path, "raw acquisition snapshot")
    raw_sha256 = hashlib.sha256(raw_payload).hexdigest()
    if artifact.get("sha256") != raw_sha256:
        raise CorpusError("raw acquisition snapshot SHA-256 does not match its manifest")
    rows: list[dict[str, object]] = []
    try:
        for number, line in enumerate(raw_payload.decode("utf-8").splitlines(), 1):
            value = json.loads(line)
            if not isinstance(value, dict):
                raise CorpusError(f"raw acquisition row {number} is not an object")
            rows.append(value)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise CorpusError(f"invalid raw acquisition snapshot: {exc}") from exc
    if artifact.get("rows") != len(rows):
        raise CorpusError("raw acquisition row count does not match its manifest")
    if _raw_counts(rows) != {
        "rows_by_country": manifest.get("rows_by_country"),
        "rows_by_country_and_source_cohort": manifest.get(
            "rows_by_country_and_source_cohort"
        ),
        "rows_by_country_source_cohort_and_diversity_shard": manifest.get(
            "rows_by_country_source_cohort_and_diversity_shard"
        ),
    }:
        raise CorpusError("raw acquisition count matrix does not match its manifest")
    args.retrieved_at = retrieved_at
    duckdb_version = str(runtime.get("duckdb_version", ""))
    if runtime != _runtime_identity(duckdb_version=duckdb_version):
        raise CorpusError("acquisition runtime identity is incomplete or has drifted")
    _assert_runtime(duckdb_version=duckdb_version)
    return rows, duckdb_version, manifest, manifest_sha256


def _manifest(
    args: argparse.Namespace,
    rows: Sequence[Mapping[str, object]],
    duckdb_version: str,
    caps: Mapping[str, int],
    *,
    script_capture: Mapping[str, object] | None = None,
    acquisition_manifest: Mapping[str, object] | None = None,
    acquisition_manifest_sha256: str | None = None,
    diagnostics_path: pathlib.Path | None = None,
    diagnostics_sha256: str | None = None,
) -> dict[str, object]:
    if script_capture is None:
        script_capture = _script_capture()
    per_country: dict[str, object] = {}
    rows_by_country = {country: 0 for country in args.countries}
    lineage_names = ("outside_chain", "common_upstream", "unknown_lineage")
    rows_by_lineage = {lineage: 0 for lineage in lineage_names}
    rows_by_country_and_lineage = {
        country: {lineage: 0 for lineage in lineage_names}
        for country in args.countries
    }
    for country in args.countries:
        country_rows = [row for row in rows if row["country"] == country]
        lineage = collections.Counter(str(row["lineage_class"]) for row in country_rows)
        rows_by_country[country] = len(country_rows)
        for lineage_name in lineage_names:
            rows_by_lineage[lineage_name] += lineage[lineage_name]
            rows_by_country_and_lineage[country][lineage_name] = lineage[lineage_name]
        per_country[country] = {
            "rows": len(country_rows),
            "lineage": dict(sorted(lineage.items())),
            "unique_cities": len({_norm(row["address"]["locality"]) for row in country_rows}),
            "unique_categories": len({
                _norm(row.get("category", "")) or "(uncategorized)"
                for row in country_rows
            }),
            "unique_networks": len({_norm(row["network"]) for row in country_rows if row["network"]}),
        }
    corpus_sha256 = _sha256_file(args.output)
    manifest: dict[str, object] = {
        "schema": CORPUS_SCHEMA,
        "corpus": args.output.name,
        "sha256": corpus_sha256,
        "rows": len(rows),
        "rows_by_country": rows_by_country,
        "rows_by_lineage": rows_by_lineage,
        "rows_by_country_and_lineage": rows_by_country_and_lineage,
        "retrieved_at": args.retrieved_at,
        "source": OVERTURE_SOURCE_URL,
        "licenses": sorted({str(row["license"]) for row in rows}),
        "lineage_policy": LINEAGE_POLICY,
        "artifact": {
            "path": args.output.name,
            "sha256": corpus_sha256,
            "rows": len(rows),
        },
        "source_details": {
            "dataset": "Overture Maps Foundation",
            "theme": OVERTURE_THEME,
            "type": OVERTURE_TYPE,
            "source_release": OVERTURE_RELEASE,
            "uri": OVERTURE_S3,
            "retrieved_at": args.retrieved_at,
            "license": (
                "MIXED: use each row's exact disclosed SourceItem license; "
                "UNKNOWN means no license was disclosed"
            ),
        },
        "selection": {
            "seed": args.seed,
            "per_country": args.per_country,
            "minimum_unknown_lineage_per_country": args.min_unknown_per_country,
            "candidate_multiplier": args.candidate_multiplier,
            "diversity_caps": dict(caps),
            "deduplication": "record_id, normalized query, and coordinate rounded to 5 decimals",
            "stages": [
                {
                    "name": "remote_candidate_pool",
                    "algorithm": "ascending SHA-256, then record_id",
                    "formula": REMOTE_CANDIDATE_HASH_FORMULA,
                    "limit_per_country": args.per_country * args.candidate_multiplier,
                    "limit_per_diversity_shard": math.ceil(
                        args.per_country
                        * args.candidate_multiplier
                        / DIVERSITY_SHARD_COUNT
                    ),
                    "limit_scope": "each country, coarse source cohort, and diversity shard",
                    "source_cohorts": list(SOURCE_COHORTS),
                    "source_cohort_formula": REMOTE_COHORT_FORMULA,
                    "diversity_shards": DIVERSITY_SHARD_COUNT,
                    "diversity_shard_formula": REMOTE_DIVERSITY_SHARD_FORMULA,
                    "execution": (
                        "one type=place S3 scan with country-coupled bbox predicates "
                        "and a bounded min_by heap per country and source cohort; SourceItem JSON is "
                        "serialized only after retention"
                    ),
                    "purpose": (
                        "bounded DuckDB-to-Python result transfer before full "
                        "Python validation"
                    ),
                },
                {
                    "name": "local_final_selection",
                    "algorithm": "ascending SHA-256, then record_id",
                    "formula": FINAL_SELECTION_HASH_FORMULA,
                    "purpose": (
                        "stable ordering after address normalization and lineage "
                        "classification, before deduplication, quota, and diversity caps"
                    ),
                },
            ],
        },
        "extraction_lineage_policy": EXTRACTION_LINEAGE_POLICY,
        "countries": per_country,
        "recipe": {
            "script": "examples/overture_places_corpus.py",
            "script_start_sha256": str(script_capture["sha256"]),
            "script_sha256": str(script_capture["sha256"]),
            "command": getattr(args, "logical_command", "python3 examples/overture_places_corpus.py"),
        },
        "runtime": _runtime_identity(duckdb_version=duckdb_version),
    }
    if acquisition_manifest is not None:
        artifact = acquisition_manifest.get("artifact", {})
        manifest["acquisition"] = {
            "artifact": artifact.get("path") if isinstance(artifact, Mapping) else None,
            "artifact_sha256": (
                artifact.get("sha256") if isinstance(artifact, Mapping) else None
            ),
            "manifest_sha256": acquisition_manifest_sha256,
            "recipe_script_start_sha256": (
                acquisition_manifest.get("recipe", {}).get("script_start_sha256")
                if isinstance(acquisition_manifest.get("recipe"), Mapping)
                else None
            ),
        }
    if diagnostics_path is not None:
        manifest["diagnostics"] = {
            "path": diagnostics_path.name,
            "sha256": diagnostics_sha256,
        }
    return manifest


def _parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    parser = argparse.ArgumentParser(description=__doc__)
    target = parser.add_mutually_exclusive_group()
    target.add_argument("--per-country", type=int, default=DEFAULT_PER_COUNTRY)
    target.add_argument(
        "--future-per-country",
        type=int,
        nargs="?",
        const=DEFAULT_FUTURE_PER_COUNTRY,
        help="future private expansion target (default when flag is present: 1000)",
    )
    parser.add_argument("--countries", nargs="+", default=list(DEFAULT_COUNTRIES))
    parser.add_argument(
        "--output",
        type=pathlib.Path,
        default=pathlib.Path("public-bench-work/overture-places-corpus.jsonl"),
    )
    acquisition = parser.add_mutually_exclusive_group()
    acquisition.add_argument(
        "--acquisition-output",
        type=pathlib.Path,
        help="fresh raw candidate snapshot path for the one network acquisition",
    )
    acquisition.add_argument(
        "--from-acquisition",
        type=pathlib.Path,
        help="verified raw snapshot to reprocess offline without DuckDB or network",
    )
    parser.add_argument("--min-unknown-per-country", type=int)
    parser.add_argument("--candidate-multiplier", type=int, default=DEFAULT_CANDIDATE_MULTIPLIER)
    parser.add_argument("--max-per-city", type=int)
    parser.add_argument("--max-per-category", type=int)
    parser.add_argument("--max-per-network", type=int)
    parser.add_argument("--seed", default=DEFAULT_SEED)
    parser.add_argument("--retrieved-at", default=_utc_now())
    parser.add_argument(
        "--acknowledge-network-read",
        action="store_true",
        help="required acknowledgement that the command reads public S3 data",
    )
    parser.add_argument(
        "--acknowledge-instrumented-acquisition",
        action="store_true",
        help="explicit acknowledgement that raw acquisition artifacts will be retained",
    )
    args = parser.parse_args(raw_argv)
    if args.future_per_country is not None:
        args.per_country = args.future_per_country
    args.countries = [country.upper() for country in args.countries]
    unsupported = sorted(set(args.countries) - set(COUNTRIES))
    if unsupported:
        parser.error(f"unsupported countries: {', '.join(unsupported)}")
    if len(args.countries) != len(set(args.countries)):
        parser.error("countries must be unique")
    if args.per_country <= 0 or args.candidate_multiplier <= 0:
        parser.error("row counts and candidate multiplier must be positive")
    if args.min_unknown_per_country is None:
        args.min_unknown_per_country = min(DEFAULT_MIN_UNKNOWN, args.per_country)
    if not 0 <= args.min_unknown_per_country <= args.per_country:
        parser.error("minimum unknown count must be between zero and per-country target")
    try:
        args.retrieved_at = _canonical_utc(args.retrieved_at)
    except CorpusError as exc:
        parser.error(str(exc))
    if args.from_acquisition is None:
        if not args.acknowledge_network_read:
            parser.error("--acknowledge-network-read is required")
        if not args.acknowledge_instrumented_acquisition:
            parser.error("--acknowledge-instrumented-acquisition is required")
    elif args.acknowledge_network_read or args.acknowledge_instrumented_acquisition:
        parser.error("offline --from-acquisition mode forbids network acknowledgements")
    args.output = args.output.resolve()
    if args.acquisition_output is None and args.from_acquisition is None:
        args.acquisition_output = args.output.with_name(args.output.stem + ".acquisition.jsonl")
    if args.acquisition_output is not None:
        args.acquisition_output = args.acquisition_output.resolve()
    if args.from_acquisition is not None:
        args.from_acquisition = args.from_acquisition.resolve()
    args.logical_command = _logical_command(raw_argv)
    return args


def _diagnostics_path(output: pathlib.Path) -> pathlib.Path:
    return output.with_suffix(output.suffix + ".diagnostics.json")


def _preflight_absent(paths: Sequence[tuple[pathlib.Path, str]]) -> None:
    for path, label in paths:
        if _path_exists_nofollow(path):
            raise CorpusError(f"refusing to replace existing {label}: {path}")


def _require_distinct_paths(paths: Sequence[pathlib.Path]) -> None:
    normalized = [os.path.normcase(os.path.abspath(path)) for path in paths]
    if len(normalized) != len(set(normalized)):
        raise CorpusError("corpus, diagnostics, acquisition, and manifest paths must be distinct")


def _verify_acquisition_pair(
    raw_path: pathlib.Path,
    manifest_path: pathlib.Path,
    manifest: Mapping[str, object],
    manifest_sha256: str,
) -> None:
    artifact = manifest.get("artifact")
    if not isinstance(artifact, Mapping):
        raise CorpusError("acquisition manifest has no artifact binding")
    if _sha256_file(raw_path) != artifact.get("sha256"):
        raise CorpusError("raw acquisition snapshot changed after verification")
    if _sha256_file(manifest_path) != manifest_sha256:
        raise CorpusError("acquisition manifest changed after verification")


def _duplicate_profile(rows: Sequence[Mapping[str, object]]) -> dict[str, int]:
    record_ids = collections.Counter(str(row["record_id"]) for row in rows)
    queries = collections.Counter(_dedupe_key(row)[1] for row in rows)
    coordinates = collections.Counter(_dedupe_key(row)[2] for row in rows)
    return {
        "rows": len(rows),
        "distinct_record_ids": len(record_ids),
        "distinct_runner_address_keys": len(queries),
        "distinct_coordinates_rounded_5dp": len(coordinates),
        "rows_in_duplicate_record_id_groups": sum(
            count for count in record_ids.values() if count > 1
        ),
        "rows_in_duplicate_address_groups": sum(
            count for count in queries.values() if count > 1
        ),
        "rows_in_duplicate_coordinate_groups": sum(
            count for count in coordinates.values() if count > 1
        ),
    }


def _selection_profile(
    candidates: Sequence[Mapping[str, object]],
    selected: Sequence[Mapping[str, object]],
    caps: Mapping[str, int],
) -> dict[str, object]:
    candidate_lineage = collections.Counter(
        str(row["lineage_class"]) for row in candidates
    )
    selected_lineage = collections.Counter(str(row["lineage_class"]) for row in selected)
    selected_groups = {name: collections.Counter() for name in caps}
    for row in selected:
        for dimension, key in _diversity_keys(row).items():
            if key:
                selected_groups[dimension][key] += 1
    return {
        "candidate_lineage": dict(sorted(candidate_lineage.items())),
        "duplicates": _duplicate_profile(candidates),
        "selected_rows": len(selected),
        "selected_lineage": dict(sorted(selected_lineage.items())),
        "maximum_observed_group_use": {
            name: max(counter.values(), default=0)
            for name, counter in selected_groups.items()
        },
        "caps": dict(caps),
    }


def _process_raw_candidates(
    args: argparse.Namespace,
    raw_rows: Sequence[Mapping[str, object]],
    caps: Mapping[str, int],
) -> tuple[list[dict[str, object]], dict[str, object], list[str]]:
    candidates: dict[str, list[dict[str, object]]] = {
        country: [] for country in args.countries
    }
    rejected = collections.Counter()
    raw_cohorts = collections.Counter()
    for raw in raw_rows:
        country = str(raw.get("country", ""))
        cohort = raw.get("source_cohort")
        shard = raw.get("diversity_shard")
        if country not in candidates:
            rejected["unsupported raw country"] += 1
            continue
        if cohort not in SOURCE_COHORTS:
            rejected["invalid acquisition source cohort"] += 1
            continue
        if not isinstance(shard, str) or not re.fullmatch(r"[0-9a-f]{2}", shard):
            rejected["invalid acquisition diversity shard"] += 1
            continue
        raw_cohorts[f"{country}:{cohort}"] += 1
        try:
            row = candidate_from_raw(raw, args.retrieved_at, args.seed)
        except CorpusError as exc:
            reason = str(exc)
            if ":" in reason:
                reason = reason.split(":", 1)[1].strip()
            rejected[reason] += 1
            continue
        candidates[country].append(row)

    selected: list[dict[str, object]] = []
    failures: list[str] = []
    country_diagnostics: dict[str, object] = {}
    for country in args.countries:
        country_rows, error = _select_country_attempt(
            candidates[country], args.per_country, args.min_unknown_per_country, caps
        )
        certificate = _selection_feasibility_certificate(
            candidates[country],
            args.per_country,
            args.min_unknown_per_country,
            caps,
        )
        if error is not None:
            failures.append(f"{country}: {error}")
        profile = _selection_profile(candidates[country], country_rows, caps)
        profile["feasibility_certificate"] = certificate
        profile["status"] = (
            "selected"
            if error is None
            else (
                "provably_infeasible"
                if certificate["proves_infeasible"]
                else "infeasible_by_selector"
            )
        )
        if error is not None:
            profile["error"] = error
        country_diagnostics[country] = profile
        if error is None:
            selected.extend(country_rows)

    selected.sort(key=lambda row: (str(row["country"]), str(row["selection_sha256"])))
    diagnostics = {
        "schema": DIAGNOSTICS_SCHEMA,
        "kind": "overture_places_selection_diagnostics",
        "retrieved_at": args.retrieved_at,
        "raw_rows": len(raw_rows),
        "raw_rows_by_country_and_source_cohort": dict(sorted(raw_cohorts.items())),
        "rejected_candidates": sum(rejected.values()),
        "rejected_by_reason": dict(sorted(rejected.items())),
        "countries": country_diagnostics,
        "selection": {
            "per_country": args.per_country,
            "minimum_unknown_lineage_per_country": args.min_unknown_per_country,
            "diversity_caps": dict(caps),
            "algorithm": (
                "constraint-scarcity ordering followed by stable SHA-256/record-id "
                "tie-breaking; failures are snapshot-local, not a proof about all Overture"
            ),
        },
        "status": "selected" if not failures else "failed_closed",
        "failures": failures,
    }
    return selected, diagnostics, failures


def main(argv: Sequence[str] | None = None) -> None:
    args = _parse_args(argv)
    _assert_runtime()
    script_capture = _script_capture()
    automatic = automatic_caps(args.per_country)
    caps = {
        "city": args.max_per_city or automatic["city"],
        "category": args.max_per_category or automatic["category"],
        "network": args.max_per_network or automatic["network"],
    }
    if any(value <= 0 for value in caps.values()):
        raise CorpusError("diversity caps must be positive")
    manifest_path = args.output.with_suffix(args.output.suffix + ".manifest.json")
    diagnostics_path = _diagnostics_path(args.output)
    raw_path = (
        args.from_acquisition
        if args.from_acquisition is not None
        else args.acquisition_output
    )
    assert raw_path is not None
    raw_manifest_path = _raw_manifest_path(raw_path)
    _require_distinct_paths(
        (args.output, manifest_path, diagnostics_path, raw_path, raw_manifest_path)
    )
    with _output_claim(args.output), _output_claim(raw_path):
        _preflight_absent(
            (
                (args.output, "corpus"),
                (manifest_path, "corpus manifest"),
                (diagnostics_path, "selection diagnostics"),
            )
        )

        acquisition_manifest: dict[str, object]
        acquisition_manifest_sha256: str
        if args.from_acquisition is not None:
            (
                raw_rows,
                duckdb_version,
                acquisition_manifest,
                acquisition_manifest_sha256,
            ) = _read_raw_snapshot(args)
        else:
            assert args.acquisition_output is not None
            _preflight_absent(
                (
                    (raw_path, "raw acquisition snapshot"),
                    (raw_manifest_path, "acquisition manifest"),
                )
            )
            raw_rows, duckdb_version, query = _load_candidates(args)
            _verify_script_capture(script_capture)
            raw_rows.sort(
                key=lambda row: (
                    str(row.get("country", "")),
                    str(row.get("source_cohort", "")),
                    str(row.get("source_order_sha256", "")),
                    str(row.get("record_id", "")),
                )
            )
            raw_payload = _jsonl_bytes(raw_rows)
            raw_sha256 = hashlib.sha256(raw_payload).hexdigest()
            acquisition_manifest = _acquisition_manifest(
                args,
                raw_rows,
                raw_path,
                raw_sha256,
                duckdb_version,
                query,
                script_capture,
            )
            _atomic_bytes_noreplace(
                raw_path, raw_payload, "raw acquisition snapshot"
            )
            _write_json(
                raw_manifest_path, acquisition_manifest, "acquisition manifest"
            )
            acquisition_manifest_sha256 = _sha256_file(raw_manifest_path)

        selected, diagnostics, failures = _process_raw_candidates(args, raw_rows, caps)
        _verify_acquisition_pair(
            raw_path,
            raw_manifest_path,
            acquisition_manifest,
            acquisition_manifest_sha256,
        )
        acquisition_artifact = acquisition_manifest.get("artifact", {})
        diagnostics["acquisition"] = {
            "artifact": (
                acquisition_artifact.get("path")
                if isinstance(acquisition_artifact, Mapping)
                else None
            ),
            "artifact_sha256": (
                acquisition_artifact.get("sha256")
                if isinstance(acquisition_artifact, Mapping)
                else None
            ),
            "manifest_sha256": acquisition_manifest_sha256,
        }
        _write_json(diagnostics_path, diagnostics, "selection diagnostics")
        diagnostics_sha256 = _sha256_file(diagnostics_path)
        _verify_script_capture(script_capture)
        _verify_acquisition_pair(
            raw_path,
            raw_manifest_path,
            acquisition_manifest,
            acquisition_manifest_sha256,
        )
        if failures:
            raise CorpusError("; ".join(failures))

        _write_jsonl(args.output, selected)
        manifest = _manifest(
            args,
            selected,
            duckdb_version,
            caps,
            script_capture=script_capture,
            acquisition_manifest=acquisition_manifest,
            acquisition_manifest_sha256=acquisition_manifest_sha256,
            diagnostics_path=diagnostics_path,
            diagnostics_sha256=diagnostics_sha256,
        )
        _write_json(manifest_path, manifest, "corpus manifest")
    print(
        json.dumps(
            {
                "corpus": str(args.output),
                "manifest": str(manifest_path),
                "sha256": manifest["artifact"]["sha256"],
                "rows": len(selected),
                "rejected_candidates": diagnostics["rejected_candidates"],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    try:
        main()
    except CorpusError as error:
        raise SystemExit(f"STOP: {error}") from error
