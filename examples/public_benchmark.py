#!/usr/bin/env python3
"""Build and run the public, provenance-bearing GridPin quality benchmark.

The benchmark deliberately keeps the truth corpus outside the repository.
Validation retains the publicly attributable schema-3 contract and also
supports a closed schema-4 mix of one pinned Overture Places acquisition and
four fixed Geofabrik extracts.  A legacy Wikidata fetcher remains available
for validate-only diagnostics.  Every accepted corpus is addressed by a
SHA-256 digest in the result file.  No private evaluation corpus is accepted.
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
import shlex
import stat
import subprocess
import sys
import tempfile
import time
import unicodedata
import urllib.error
import urllib.parse
import urllib.request


COUNTRIES = {
    # European product extents.  Keeping the bounds explicit prevents an item
    # with only a country claim from silently representing an overseas or
    # out-of-country coordinate that the compared sheet does not cover.
    "FR": {
        "qid": "Q142",
        "language": "fr",
        "name": "France",
        "bounds": (-5.5, 41.0, 9.8, 51.5),
    },
    "IT": {
        "qid": "Q38",
        "language": "it",
        "name": "Italy",
        "bounds": (6.6, 35.3, 18.6, 47.2),
    },
    "NL": {
        "qid": "Q55",
        "language": "nl",
        "name": "Netherlands",
        "bounds": (3.2, 50.7, 7.3, 53.7),
    },
    "RS": {
        "qid": "Q403",
        "language": "sr",
        "name": "Serbia",
        "bounds": (18.7, 41.8, 23.1, 46.3),
    },
}
WDQS = "https://query.wikidata.org/sparql"
WIKIDATA_LICENSE = "CC0-1.0"
WIKIDATA_DATASET = "Wikidata"
WIKIDATA_THEME = "entities"
WIKIDATA_TYPE = "item"
WIKIDATA_SOURCE_RELEASE = "live-query"
OVERTURE_DATASET = "Overture Maps Foundation"
OVERTURE_THEME = "places"
OVERTURE_TYPE = "place"
OVERTURE_RELEASE = "2026-06-17.0"
OVERTURE_S3 = (
    "s3://overturemaps-us-west-2/release/"
    f"{OVERTURE_RELEASE}/theme={OVERTURE_THEME}/type={OVERTURE_TYPE}/*.parquet"
)
OVERTURE_RAW_SHA256 = "1697a275dfca8e1b5bb3577dd65200d9b6ed3f335b02ec73a748eac2d94707b9"
OVERTURE_SOURCE_ID = "overture_places_2026_06_17_0"
OVERTURE_SOURCE_FAMILY = "overture_places"
OVERTURE_SNAPSHOT_AT = "2026-08-01T20:49:04Z"
OVERTURE_ACQUISITION_MANIFEST_SHA256 = (
    "ab9b3ea830189ac3d7eaa9d356035503a4ae1d8c698bdc5a74b752d5f8aece16"
)
GEOFABRIK_SNAPSHOT_AT = "2026-07-01T20:22:00Z"
OSM_SOURCE_FAMILY = "openstreetmap"
OSM_LICENSE = "ODbL-1.0"
TRUTH_SCHEMA = 3
MULTI_SOURCE_TRUTH_SCHEMA = 4
LINEAGE_POLICY = "disclosed-public-coordinate-lineage-v1"
LINEAGE_CLASSES = (
    "outside_chain",
    "common_upstream",
    "unknown_lineage",
)
DEFAULT_MINIMUM = 300
DEFAULT_UNKNOWN_MINIMUM = 150
DEFAULT_DISTANCE_M = 300.0
FOURSQUARE_OUTSIDE_230_PROFILE = "foursquare-outside-chain-230-v1"
FOURSQUARE_OUTSIDE_230_COUNTS = {
    "FR": 43,
    "IT": 129,
    "NL": 17,
    "RS": 41,
}
FOURSQUARE_OUTSIDE_230_MINIMUM = min(FOURSQUARE_OUTSIDE_230_COUNTS.values())
FOURSQUARE_OUTSIDE_230_WITNESS_SHA256 = (
    "bfc8a5c5805629915dd4f4c72a3ed0c06b1fa8a76e1f1caad1ed0d24950b86cd"
)
FOURSQUARE_OUTSIDE_230_SELECTED_RECORD_IDS_SHA256 = (
    "ddec06a8e8e3b72675c315770f6e6c40417373cf4c431315dbfb5d3902ef3bd7"
)
FOURSQUARE_OUTSIDE_230_MARKERS_SHA256 = (
    "f5d01d85f032ba0bacdeaf6caf274124f4776172607a5e652e01e2e141260c34"
)
FOURSQUARE_OUTSIDE_230_CORPUS_SHA256 = (
    "0489a8e39b936f97a004bf9b7d028705d2afea9a572a713d85b568025532b6ce"
)
FOURSQUARE_OUTSIDE_230_CAPS = {
    "municipality": 12,
    "street": 2,
    "cell_0_01_degree": 3,
    "category": 24,
    "network": 9,
}
RESPONSE_CACHE_SCHEMA = 3
STATUS_EVIDENCE_SCHEMA = 1
SOURCE_SEPARATION_SCHEMA = 1
USER_AGENT_PRODUCT = "GridPin-public-benchmark/1.0"
SHEET_META_ALLOWLIST = (
    "country",
    "layer",
    "source_release",
    "license",
    "sources",
)

# Schema 4 is deliberately a closed, hash-bound acquisition set.  These are
# required pin subsets, not whole catalog entries: producers may retain more
# diagnostics, but may not replace any acquisition identity below.
V4_SOURCE_CATALOG = {
    OVERTURE_SOURCE_ID: {
        "source_id": OVERTURE_SOURCE_ID,
        "family": OVERTURE_SOURCE_FAMILY,
        "dataset": OVERTURE_DATASET,
        "theme": OVERTURE_THEME,
        "type": OVERTURE_TYPE,
        "source_release": OVERTURE_RELEASE,
        "snapshot_at": OVERTURE_SNAPSHOT_AT,
        "license": "MIXED: exact license is retained per SourceItem and row",
        "public_uri": OVERTURE_S3,
        "coverage_scope": "country extents: FR, IT, NL, RS",
        "retained_input": {
            "logical_name": "overture-places-2026-06-17.0-instrumented-v3.acquisition.jsonl",
            "sha256": OVERTURE_RAW_SHA256,
            "bytes": 4_233_660,
        },
        "acquisition_manifest": {
            "logical_name": (
                "overture-places-2026-06-17.0-instrumented-v3.acquisition.jsonl."
                "manifest.json"
            ),
            "sha256": OVERTURE_ACQUISITION_MANIFEST_SHA256,
            "bytes": 30_802,
        },
    },
    "osm_geofabrik_fr_alsace_260701": {
        "source_id": "osm_geofabrik_fr_alsace_260701",
        "family": OSM_SOURCE_FAMILY,
        "dataset": "OpenStreetMap via Geofabrik",
        "theme": "addresses",
        "type": "node",
        "source_release": "geofabrik-replication-4830@2026-07-01T20:22:00Z",
        "snapshot_at": GEOFABRIK_SNAPSHOT_AT,
        "license": OSM_LICENSE,
        "public_uri": "https://download.geofabrik.de/europe/france/alsace-260701.osm.pbf",
        "region": "alsace",
        "coverage_scope": "regional: Alsace",
        "attribution": "© OpenStreetMap contributors; ODbL 1.0",
        "copyright_url": "https://www.openstreetmap.org/copyright",
        "retained_input": {
            "logical_name": "alsace-260701.osm.pbf",
            "sha256": "f8a63f9a31864821a16fa1fd1fd2626a587c4ea2d780a2d863bfa361d19bfaa7",
            "bytes": 129_643_154,
        },
        "pbf": {
            "crc32": "158d2a49",
            "data_bbox": [6.0352737, 47.2659546, 8.4734136, 49.6128311],
            "header_boxes": [[6.838921, 47.3845, 8.306437, 49.208014]],
            "object_counts": {
                "changesets": 0,
                "nodes": 12_558_192,
                "relations": 44_969,
                "ways": 1_975_498,
            },
            "format": "PBF",
            "replication_base_url": "https://download.geofabrik.de/europe/france/alsace-updates",
            "replication_sequence": 4_830,
            "snapshot_at": GEOFABRIK_SNAPSHOT_AT,
            "multiple_versions": False,
            "objects_ordered": True,
        },
    },
    "osm_geofabrik_it_isole_260701": {
        "source_id": "osm_geofabrik_it_isole_260701",
        "family": OSM_SOURCE_FAMILY,
        "dataset": "OpenStreetMap via Geofabrik",
        "theme": "addresses",
        "type": "node",
        "source_release": "geofabrik-replication-3893@2026-07-01T20:22:00Z",
        "snapshot_at": GEOFABRIK_SNAPSHOT_AT,
        "license": OSM_LICENSE,
        "public_uri": "https://download.geofabrik.de/europe/italy/isole-260701.osm.pbf",
        "region": "isole",
        "coverage_scope": "regional: Isole",
        "attribution": "© OpenStreetMap contributors; ODbL 1.0",
        "copyright_url": "https://www.openstreetmap.org/copyright",
        "retained_input": {
            "logical_name": "isole-260701.osm.pbf",
            "sha256": "b820ee216ef76b326bf1306ea946abfbfe58bdf08db9147289cf556d22665e88",
            "bytes": 212_733_976,
        },
        "pbf": {
            "crc32": "d131664",
            "data_bbox": [-4.950883, 31.21, 29.9753085, 45.4583548],
            "header_boxes": [[7.418403, 34.3753, 15.70887, 41.51893]],
            "object_counts": {
                "changesets": 0,
                "nodes": 26_716_822,
                "relations": 44_019,
                "ways": 3_473_533,
            },
            "format": "PBF",
            "replication_base_url": "https://download.geofabrik.de/europe/italy/isole-updates",
            "replication_sequence": 3_893,
            "snapshot_at": GEOFABRIK_SNAPSHOT_AT,
            "multiple_versions": False,
            "objects_ordered": True,
        },
    },
    "osm_geofabrik_nl_drenthe_260701": {
        "source_id": "osm_geofabrik_nl_drenthe_260701",
        "family": OSM_SOURCE_FAMILY,
        "dataset": "OpenStreetMap via Geofabrik",
        "theme": "addresses",
        "type": "node",
        "source_release": "geofabrik-replication-2774@2026-07-01T20:22:00Z",
        "snapshot_at": GEOFABRIK_SNAPSHOT_AT,
        "license": OSM_LICENSE,
        "public_uri": "https://download.geofabrik.de/europe/netherlands/drenthe-260701.osm.pbf",
        "region": "drenthe",
        "coverage_scope": "regional: Drenthe",
        "attribution": "© OpenStreetMap contributors; ODbL 1.0",
        "copyright_url": "https://www.openstreetmap.org/copyright",
        "retained_input": {
            "logical_name": "drenthe-260701.osm.pbf",
            "sha256": "2814290012b08420820e2ef47373156707128de68a2b15783302e8a6feafa326",
            "bytes": 62_699_767,
        },
        "pbf": {
            "crc32": "6fb240f3",
            "data_bbox": [6.0724332, 51.2541943, 7.3178414, 53.6675386],
            "header_boxes": [[6.118592, 52.582309, 7.405064, 53.20513]],
            "object_counts": {
                "changesets": 0,
                "nodes": 6_418_715,
                "relations": 14_716,
                "ways": 779_507,
            },
            "format": "PBF",
            "replication_base_url": "https://download.geofabrik.de/europe/netherlands/drenthe-updates",
            "replication_sequence": 2_774,
            "snapshot_at": GEOFABRIK_SNAPSHOT_AT,
            "multiple_versions": False,
            "objects_ordered": True,
        },
    },
    "osm_geofabrik_rs_serbia_260701": {
        "source_id": "osm_geofabrik_rs_serbia_260701",
        "family": OSM_SOURCE_FAMILY,
        "dataset": "OpenStreetMap via Geofabrik",
        "theme": "addresses",
        "type": "node",
        "source_release": "geofabrik-replication-4835@2026-07-01T20:22:00Z",
        "snapshot_at": GEOFABRIK_SNAPSHOT_AT,
        "license": OSM_LICENSE,
        "public_uri": "https://download.geofabrik.de/europe/serbia-260701.osm.pbf",
        "region": "serbia",
        "coverage_scope": "national: Serbia",
        "attribution": "© OpenStreetMap contributors; ODbL 1.0",
        "copyright_url": "https://www.openstreetmap.org/copyright",
        "retained_input": {
            "logical_name": "serbia-260701.osm.pbf",
            "sha256": "0d5e526a7411e6a0dd7400bf188392d79de17477bd0612dee216a5a255fb83d0",
            "bytes": 236_966_213,
        },
        "pbf": {
            "crc32": "211839c9",
            "data_bbox": [18.1309958, 41.7753644, 25.4041291, 47.0234494],
            "header_boxes": [[18.808937, 42.229789, 23.010349, 46.192072]],
            "object_counts": {
                "changesets": 0,
                "nodes": 29_171_014,
                "relations": 44_907,
                "ways": 3_208_003,
            },
            "format": "PBF",
            "replication_base_url": "https://download.geofabrik.de/europe/serbia-updates",
            "replication_sequence": 4_835,
            "snapshot_at": GEOFABRIK_SNAPSHOT_AT,
            "multiple_versions": False,
            "objects_ordered": True,
        },
    },
}
V4_SOURCE_COUNTRIES = {
    OVERTURE_SOURCE_ID: tuple(COUNTRIES),
    "osm_geofabrik_fr_alsace_260701": ("FR",),
    "osm_geofabrik_it_isole_260701": ("IT",),
    "osm_geofabrik_nl_drenthe_260701": ("NL",),
    "osm_geofabrik_rs_serbia_260701": ("RS",),
}
V4_ROWS_BY_SOURCE = {
    OVERTURE_SOURCE_ID: 200,
    "osm_geofabrik_fr_alsace_260701": 250,
    "osm_geofabrik_it_isole_260701": 250,
    "osm_geofabrik_nl_drenthe_260701": 250,
    "osm_geofabrik_rs_serbia_260701": 250,
}
V4_ROWS_BY_COUNTRY_AND_SOURCE = {
    country: {
        source_id: (
            50
            if source_id == OVERTURE_SOURCE_ID
            else 250 if country in V4_SOURCE_COUNTRIES[source_id] else 0
        )
        for source_id in V4_SOURCE_CATALOG
    }
    for country in COUNTRIES
}
SERVICE_ATTRIBUTION = {
    "photon": "Photon search over OpenStreetMap-derived data; OpenStreetMap contributors, ODbL",
    "nominatim": "Nominatim search over OpenStreetMap data; OpenStreetMap contributors, ODbL",
}

# These are the exact ancestors recorded for each indexed country sheet.  A
# generic open-data marker is not enough: OSM and Overture remain unknown, and
# OpenAddresses is common only for the IT/NL/RS sheets.  This policy must stay
# byte-for-byte equivalent in meaning to the primary Overture Places recipe.
COUNTRY_COMMON_LINEAGE = {
    "FR": (
        (
            "Base Adresse Nationale (BAN)",
            ("base adresse nationale", "ban"),
            ("adresse.data.gouv.fr",),
        ),
    ),
    "IT": (
        (
            "Archivio Nazionale dei Numeri Civici e delle Strade Urbane (ANNCSU)",
            ("anncsu", "archivio nazionale dei numeri civici"),
            (),
        ),
        (
            "OpenAddresses",
            ("openaddresses", "open addresses"),
            ("openaddresses.io", "results.openaddresses.io"),
        ),
    ),
    "NL": (
        (
            "Basisregistratie Adressen en Gebouwen (BAG/Kadaster)",
            ("basisregistratie adressen en gebouwen", "kadaster", "bag"),
            ("kadaster.nl",),
        ),
        (
            "OpenAddresses",
            ("openaddresses", "open addresses"),
            ("openaddresses.io", "results.openaddresses.io"),
        ),
    ),
    "RS": (
        (
            "Republic Geodetic Authority (RGZ)",
            (
                "republic geodetic authority",
                "republicki geodetski zavod",
                "republički geodetski zavod",
                "rgz",
            ),
            ("rgz.gov.rs",),
        ),
        (
            "OpenAddresses",
            ("openaddresses", "open addresses"),
            ("openaddresses.io", "results.openaddresses.io"),
        ),
    ),
}


class BenchmarkError(RuntimeError):
    """A fail-closed benchmark contract violation."""


class _NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_NO_REDIRECT_OPENER = urllib.request.build_opener(_NoRedirectHandler())


def _urlopen_no_redirect(request: urllib.request.Request, timeout: float):
    return _NO_REDIRECT_OPENER.open(request, timeout=timeout)


def _utc_now() -> str:
    # Cache timestamps are also rate-limit checkpoints.  Preserve sub-second
    # precision so a resumed process cannot accidentally shorten a required
    # inter-request pause by almost a second.
    return dt.datetime.now(dt.timezone.utc).isoformat()


def _sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _logical_path(path: pathlib.Path) -> str:
    """Return a reproducible path label without leaking a local absolute path."""
    resolved = path.expanduser().resolve()
    repository = pathlib.Path(__file__).resolve().parents[1]
    try:
        return resolved.relative_to(repository).as_posix()
    except ValueError:
        return resolved.name


def _sanitize_json_paths(value):
    if isinstance(value, dict):
        return {key: _sanitize_json_paths(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_sanitize_json_paths(item) for item in value]
    if isinstance(value, tuple):
        return [_sanitize_json_paths(item) for item in value]
    if isinstance(value, str):
        parsed = urllib.parse.urlsplit(value)
        if parsed.scheme.lower() in {"http", "https"} and parsed.netloc:
            return value
        if parsed.scheme.lower() == "file":
            return "<redacted-local-path>"
        candidate = pathlib.Path(value).expanduser()
        if candidate.is_absolute():
            return _logical_path(candidate)
        value = re.sub(r"file://[^\s\"'`,;)}\]]+", "<redacted-local-path>", value)
        value = re.sub(
            r"(?<![A-Za-z0-9:/])/(?!/)[^\s\"'`,;:)}\]]+",
            "<redacted-local-path>",
            value,
        )
        value = re.sub(
            r"(?<![A-Za-z0-9])[A-Za-z]:[\\/][^\s\"'`,;:)}\]]+",
            "<redacted-local-path>",
            value,
        )
        value = re.sub(
            r"(?<![A-Za-z0-9._%+-])[A-Za-z0-9._%+-]+@"
            r"[A-Za-z0-9.-]+\.[A-Za-z]{2,}(?![A-Za-z0-9.-])",
            "<redacted-contact>",
            value,
        )
    return value


def _path_exists_nofollow(path: pathlib.Path) -> bool:
    try:
        os.lstat(path)
    except FileNotFoundError:
        return False
    return True


def _require_regular_nofollow(
    path: pathlib.Path,
    label: str,
    *,
    single_link: bool = False,
) -> os.stat_result:
    try:
        info = os.lstat(path)
    except OSError as exc:
        raise BenchmarkError(f"cannot inspect {label} {path}: {exc}") from exc
    if not stat.S_ISREG(info.st_mode):
        raise BenchmarkError(f"{label} must be a regular file and must not be a symlink: {path}")
    if single_link and info.st_nlink != 1:
        raise BenchmarkError(f"{label} must have exactly one filesystem link: {path}")
    return info


def _open_regular_nofollow(
    path: pathlib.Path,
    flags: int,
    label: str,
    *,
    single_link: bool = False,
) -> int:
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags | nofollow)
    except OSError as exc:
        raise BenchmarkError(f"cannot open {label} {path} without following symlinks: {exc}") from exc
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            raise BenchmarkError(f"{label} must be a regular file: {path}")
        if single_link and info.st_nlink != 1:
            raise BenchmarkError(f"{label} must have exactly one filesystem link: {path}")
        current = os.lstat(path)
        if (current.st_dev, current.st_ino) != (info.st_dev, info.st_ino):
            raise BenchmarkError(f"{label} changed while it was being opened: {path}")
        return fd
    except BaseException:
        os.close(fd)
        raise


def _fsync_directory(path: pathlib.Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    try:
        fd = os.open(path, flags)
    except OSError:
        return
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _same_file_identity(path: pathlib.Path, identity: os.stat_result) -> bool:
    try:
        current = os.lstat(path)
    except FileNotFoundError:
        return False
    return (current.st_dev, current.st_ino) == (identity.st_dev, identity.st_ino)


@contextlib.contextmanager
def _exclusive_claim(
    target: pathlib.Path,
    label: str,
    *,
    require_target_absent: bool,
):
    """Claim a target with a persistent, non-symlink flock inode.

    The lock file is deliberately never unlinked: unlinking a shared lock inode
    can split waiters across old and new inodes.  Cleanup means truncating the
    diagnostic payload and releasing the kernel lock, so failures do not block
    a later run.
    """
    target.parent.mkdir(parents=True, exist_ok=True)
    lock = target.with_name(target.name + ".lock")
    flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(lock, flags, 0o600)
    except OSError as exc:
        raise BenchmarkError(f"cannot claim {label} {target}: {exc}") from exc
    locked = False
    try:
        identity = os.fstat(fd)
        if not stat.S_ISREG(identity.st_mode) or identity.st_nlink != 1:
            raise BenchmarkError(f"{label} lock must be a single-link regular file: {lock}")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise BenchmarkError(f"{label} is already claimed by another writer: {lock}") from exc
        locked = True
        if not _same_file_identity(lock, identity):
            raise BenchmarkError(f"{label} lock path changed during acquisition: {lock}")
        os.ftruncate(fd, 0)
        os.lseek(fd, 0, os.SEEK_SET)
        payload = json.dumps({"schema": 1, "target": target.name, "pid": os.getpid()}) + "\n"
        os.write(fd, payload.encode("utf-8"))
        os.fsync(fd)
        if require_target_absent and _path_exists_nofollow(target):
            raise BenchmarkError(
                f"{label} already exists; choose a fresh path so it cannot be replaced: {target}"
            )
        yield
    finally:
        cleanup_error: OSError | None = None
        if locked:
            try:
                os.ftruncate(fd, 0)
                os.fsync(fd)
            except OSError as exc:
                cleanup_error = exc
            try:
                fcntl.flock(fd, fcntl.LOCK_UN)
            except OSError as exc:
                cleanup_error = cleanup_error or exc
        os.close(fd)
        if cleanup_error is not None and sys.exc_info()[0] is None:
            raise BenchmarkError(f"cannot clean and release {label} lock {lock}: {cleanup_error}")


def _publish_existing_noreplace(
    source: pathlib.Path,
    target: pathlib.Path,
    label: str,
    *,
    expected_source_identity: os.stat_result,
) -> None:
    """Atomically publish an existing regular file without replacing a peer's file."""
    source_info = _require_regular_nofollow(source, f"{label} candidate")
    expected_key = (expected_source_identity.st_dev, expected_source_identity.st_ino)
    if (source_info.st_dev, source_info.st_ino) != expected_key:
        raise BenchmarkError(f"{label} candidate ownership changed before publication")
    if source_info.st_nlink != 1:
        raise BenchmarkError(f"{label} candidate must have exactly one filesystem link: {source}")
    try:
        os.link(source, target, follow_symlinks=False)
    except FileExistsError as exc:
        raise BenchmarkError(
            f"{label} appeared during publication and was preserved: {target}"
        ) from exc
    except OSError as exc:
        raise BenchmarkError(f"cannot publish {label} {target}: {exc}") from exc
    published = _require_regular_nofollow(target, label)
    if (published.st_dev, published.st_ino) != expected_key:
        # Do not attempt a check-then-unlink cleanup here: a peer could replace
        # the authoritative target between those operations and have its file
        # deleted.  Fail closed while preserving whatever won that pathname.
        raise BenchmarkError(f"published {label} has an unexpected source identity: {target}")
    _fsync_directory(target.parent)


def _atomic_json_noreplace(
    path: pathlib.Path,
    payload: dict,
    *,
    before_publish=None,
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, raw_tmp = tempfile.mkstemp(prefix=path.name + ".", suffix=".part", dir=path.parent)
    tmp = pathlib.Path(raw_tmp)
    tmp_identity = os.fstat(fd)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        if before_publish is not None:
            before_publish()
        _publish_existing_noreplace(
            tmp,
            path,
            "JSON output",
            expected_source_identity=tmp_identity,
        )
    finally:
        if _same_file_identity(tmp, tmp_identity):
            tmp.unlink()


def _atomic_jsonl_noreplace(path: pathlib.Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, raw_tmp = tempfile.mkstemp(prefix=path.name + ".", suffix=".part", dir=path.parent)
    tmp = pathlib.Path(raw_tmp)
    tmp_identity = os.fstat(fd)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            for row in rows:
                handle.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        _publish_existing_noreplace(
            tmp,
            path,
            "JSONL output",
            expected_source_identity=tmp_identity,
        )
    finally:
        if _same_file_identity(tmp, tmp_identity):
            tmp.unlink()


def _capture_file(path: pathlib.Path, label: str) -> dict:
    resolved = path.expanduser().resolve()
    try:
        info = resolved.stat()
        if not stat.S_ISREG(info.st_mode):
            raise BenchmarkError(f"{label} is not a regular file: {resolved}")
        digest = _sha256(resolved)
    except OSError as exc:
        raise BenchmarkError(f"cannot capture {label} {resolved}: {exc}") from exc
    return {
        "_path": resolved,
        "label": label,
        "path": _logical_path(resolved),
        "sha256": digest,
    }


def _capture_regular_nofollow(
    path: pathlib.Path,
    label: str,
    *,
    single_link: bool,
) -> dict:
    absolute = pathlib.Path(os.path.abspath(path.expanduser()))
    digest = _sha256_regular(absolute, label, single_link=single_link)
    return {
        "_path": absolute,
        "label": label,
        "path": _logical_path(absolute),
        "sha256": digest,
        "nofollow": True,
        "single_link": single_link,
    }


def _verify_capture(capture: dict) -> None:
    path = capture["_path"]
    try:
        if capture.get("nofollow"):
            current = _sha256_regular(
                path,
                capture["label"],
                single_link=bool(capture.get("single_link")),
            )
        else:
            current = _sha256(path)
    except OSError as exc:
        raise BenchmarkError(f"{capture['label']} became unreadable during benchmark: {exc}") from exc
    if current != capture["sha256"]:
        raise BenchmarkError(
            f"{capture['label']} changed during benchmark: captured {capture['sha256']}, now {current}"
        )


def _verify_captures(captures: list[dict]) -> None:
    for capture in captures:
        _verify_capture(capture)


def _public_capture(capture: dict) -> dict:
    return {"path": capture["path"], "sha256": capture["sha256"]}


def _validate_utc_timestamp(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise BenchmarkError(f"{label} must be a non-empty ISO8601 UTC timestamp")
    candidate = value.strip()
    try:
        parsed = dt.datetime.fromisoformat(candidate.replace("Z", "+00:00"))
    except ValueError as exc:
        raise BenchmarkError(f"{label} must be an ISO8601 UTC timestamp") from exc
    if parsed.tzinfo is None or parsed.utcoffset() != dt.timedelta(0):
        raise BenchmarkError(f"{label} must be an ISO8601 UTC timestamp")
    if parsed > dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=5):
        raise BenchmarkError(f"{label} must not be in the future")
    return candidate


_EMAIL_RE = re.compile(r"^[^@\s()<>]+@[^@\s()<>]+\.[^@\s()<>]+$")


def _valid_contact(contact: str) -> bool:
    if len(contact) > 200 or any(ord(character) < 0x20 or ord(character) == 0x7F for character in contact):
        return False
    if _EMAIL_RE.fullmatch(contact):
        return True
    parsed = urllib.parse.urlsplit(contact)
    return (
        parsed.scheme.lower() in {"http", "https"}
        and bool(parsed.hostname)
        and parsed.username is None
        and parsed.password is None
    )


def _user_agent(contact: str) -> str:
    cleaned = _clean_text(contact)
    if not cleaned or not _valid_contact(cleaned):
        raise BenchmarkError("contact must be a non-empty email address or absolute HTTP(S) URL")
    return f"{USER_AGENT_PRODUCT} ({cleaned})"


def _validate_user_agent(value: object) -> str:
    if not isinstance(value, str):
        raise BenchmarkError(
            f"--user-agent must be exactly '{USER_AGENT_PRODUCT} (<email-or-absolute-url>)'"
        )
    if len(value) > 256 or any(ord(character) < 0x20 or ord(character) == 0x7F for character in value):
        raise BenchmarkError("--user-agent must be printable and no longer than 256 characters")
    prefix = USER_AGENT_PRODUCT + " ("
    if not value.startswith(prefix) or not value.endswith(")"):
        raise BenchmarkError(
            f"--user-agent must be exactly '{USER_AGENT_PRODUCT} (<email-or-absolute-url>)'"
        )
    contact = value[len(prefix):-1]
    if not contact or contact != contact.strip() or not _valid_contact(contact):
        raise BenchmarkError("--user-agent contact must be a non-empty email or absolute HTTP(S) URL")
    return value


def _truth_manifest_path(path: pathlib.Path) -> pathlib.Path:
    return path.with_suffix(path.suffix + ".manifest.json")


def _manifest_text(manifest_path: pathlib.Path, label: str, value: object) -> str:
    if not isinstance(value, str) or not _clean_text(value):
        raise BenchmarkError(f"truth manifest {label} must be non-empty text: {manifest_path}")
    return value


def _manifest_source_uri(manifest_path: pathlib.Path, value: object) -> str:
    uri = _manifest_text(manifest_path, "source_details.uri", value)
    parsed = urllib.parse.urlsplit(uri)
    valid_web = (
        parsed.scheme.lower() in {"http", "https"}
        and bool(parsed.hostname)
        and parsed.username is None
        and parsed.password is None
    )
    valid_s3 = (
        parsed.scheme.lower() == "s3"
        and bool(parsed.netloc)
        and parsed.username is None
        and parsed.password is None
        and parsed.path.startswith("/")
    )
    if not (valid_web or valid_s3):
        raise BenchmarkError(
            f"truth manifest source_details.uri must be an absolute public HTTP(S) or S3 URI: {manifest_path}"
        )
    return uri


def _manifest_source_details(manifest_path: pathlib.Path, manifest: dict) -> dict:
    details = manifest.get("source_details")
    if not isinstance(details, dict):
        raise BenchmarkError(
            f"truth manifest source_details must be an object: {manifest_path}"
        )
    required = {
        "dataset",
        "theme",
        "type",
        "source_release",
        "uri",
        "retrieved_at",
        "license",
    }
    missing = sorted(required - details.keys())
    if missing:
        raise BenchmarkError(
            "truth manifest source_details missing fields: "
            + ", ".join(missing)
        )
    for field in ("dataset", "theme", "type", "source_release", "license"):
        _manifest_text(manifest_path, f"source_details.{field}", details[field])
    _manifest_source_uri(manifest_path, details["uri"])
    _validate_utc_timestamp(
        details["retrieved_at"], "truth manifest source_details.retrieved_at"
    )
    return details


def _manifest_recipe(manifest_path: pathlib.Path, manifest: dict) -> dict:
    recipe = manifest.get("recipe")
    if not isinstance(recipe, dict):
        raise BenchmarkError(f"truth manifest recipe must be an object: {manifest_path}")
    required = {"script", "script_sha256", "command"}
    missing = sorted(required - recipe.keys())
    if missing:
        raise BenchmarkError(
            "truth manifest recipe missing fields: " + ", ".join(missing)
        )
    _manifest_text(manifest_path, "recipe.script", recipe["script"])
    script_sha256 = _manifest_text(
        manifest_path, "recipe.script_sha256", recipe["script_sha256"]
    )
    if re.fullmatch(r"[0-9a-f]{64}", script_sha256) is None:
        raise BenchmarkError(
            f"truth manifest recipe.script_sha256 must be lowercase SHA-256: {manifest_path}"
        )
    _manifest_text(manifest_path, "recipe.command", recipe["command"])
    return recipe


def _corpus_schema(rows: list[dict]) -> int:
    """Select v4 only from an explicit first-row marker; everything else stays v3."""
    if rows and type(rows[0].get("schema")) is int and rows[0]["schema"] == MULTI_SOURCE_TRUTH_SCHEMA:
        return MULTI_SOURCE_TRUTH_SCHEMA
    return TRUTH_SCHEMA


def _require_pinned_subset(actual: object, expected: object, label: str) -> None:
    if isinstance(expected, dict):
        if not isinstance(actual, dict):
            raise BenchmarkError(f"{label} must be an object")
        missing = sorted(set(expected) - set(actual))
        if missing:
            raise BenchmarkError(f"{label} missing pinned fields: {', '.join(missing)}")
        for key, expected_value in expected.items():
            _require_pinned_subset(actual[key], expected_value, f"{label}.{key}")
        return
    if isinstance(expected, list):
        if not isinstance(actual, list) or len(actual) != len(expected):
            raise BenchmarkError(f"{label} does not match its fixed pin")
        for index, expected_value in enumerate(expected):
            _require_pinned_subset(actual[index], expected_value, f"{label}[{index}]")
        return
    if actual != expected or type(actual) is not type(expected):
        raise BenchmarkError(
            f"{label} does not match its fixed pin: expected {expected!r}, got {actual!r}"
        )


def _validate_fixed_artifact(value: object, label: str, expected: dict) -> dict:
    if not isinstance(value, dict):
        raise BenchmarkError(f"{label} must be an object")
    _require_pinned_subset(value, expected, label)
    if re.fullmatch(r"[0-9a-f]{64}", value["sha256"]) is None:
        raise BenchmarkError(f"{label}.sha256 must be lowercase SHA-256")
    if type(value["bytes"]) is not int or value["bytes"] <= 0:
        raise BenchmarkError(f"{label}.bytes must be a positive integer")
    return value


def _validate_v4_source_catalog(manifest_path: pathlib.Path, value: object) -> dict:
    if not isinstance(value, dict):
        raise BenchmarkError(f"truth manifest source_catalog must be an object: {manifest_path}")
    expected_ids = set(V4_SOURCE_CATALOG)
    actual_ids = set(value)
    if actual_ids != expected_ids:
        missing = sorted(expected_ids - actual_ids)
        extra = sorted(actual_ids - expected_ids)
        raise BenchmarkError(
            "truth manifest source_catalog IDs do not match the fixed acquisition set: "
            f"missing={missing}, extra={extra}"
        )
    for source_id, expected in V4_SOURCE_CATALOG.items():
        entry = value.get(source_id)
        if not isinstance(entry, dict):
            raise BenchmarkError(f"truth manifest source_catalog.{source_id} must be an object")
        label = f"truth manifest source_catalog.{source_id}"
        _require_pinned_subset(entry, expected, label)
        _validate_utc_timestamp(
            entry["snapshot_at"],
            f"{label}.snapshot_at",
        )
        _manifest_source_uri(manifest_path, entry["public_uri"])
        _validate_fixed_artifact(
            entry["retained_input"], f"{label}.retained_input", expected["retained_input"]
        )
        if entry["family"] == OSM_SOURCE_FAMILY:
            pbf = entry.get("pbf")
            if not isinstance(pbf, dict):
                raise BenchmarkError(f"{label}.pbf must be an object")
            required_pbf = {
                "crc32",
                "data_bbox",
                "header_boxes",
                "object_counts",
                "format",
                "replication_base_url",
                "replication_sequence",
                "snapshot_at",
                "multiple_versions",
                "objects_ordered",
            }
            missing_pbf = sorted(required_pbf - pbf.keys())
            if missing_pbf:
                raise BenchmarkError(f"{label}.pbf missing fields: {', '.join(missing_pbf)}")
            if re.fullmatch(r"[0-9a-f]{1,8}", pbf["crc32"]) is None:
                raise BenchmarkError(f"{label}.pbf.crc32 must be lowercase CRC32")
            if not isinstance(pbf["object_counts"], dict) or any(
                type(count) is not int or count < 0 for count in pbf["object_counts"].values()
            ):
                raise BenchmarkError(f"{label}.pbf.object_counts must contain non-negative integers")
        else:
            acquisition = entry.get("acquisition_manifest")
            _validate_fixed_artifact(
                acquisition,
                f"{label}.acquisition_manifest",
                expected["acquisition_manifest"],
            )
            runtime = entry.get("runtime")
            if not isinstance(runtime, dict):
                raise BenchmarkError(f"{label}.runtime must be an object")
            _manifest_text(manifest_path, f"{label}.runtime.duckdb_version", runtime.get("duckdb_version"))
    return value


def _source_counts(
    rows: list[dict], source_ids: tuple[str, ...] | list[str]
) -> tuple[dict[str, int], dict[str, dict[str, int]], dict[str, dict[str, dict[str, int]]]]:
    by_source = {source_id: 0 for source_id in source_ids}
    by_country_and_source = {
        country: {source_id: 0 for source_id in source_ids}
        for country in COUNTRIES
    }
    by_country_source_lineage = {
        country: {
            source_id: {lineage: 0 for lineage in LINEAGE_CLASSES}
            for source_id in source_ids
        }
        for country in COUNTRIES
    }
    for row in rows:
        source_id = row["truth_source_id"]
        country = row["country"]
        lineage = row["lineage_class"]
        if source_id not in by_source:
            raise BenchmarkError(f"truth row names unsupported truth_source_id {source_id!r}")
        by_source[source_id] += 1
        by_country_and_source[country][source_id] += 1
        by_country_source_lineage[country][source_id][lineage] += 1
    return by_source, by_country_and_source, by_country_source_lineage


def _validate_v4_row_source(path: pathlib.Path, line_no: int, row: dict) -> None:
    required = {
        "truth_source_id",
        "truth_source_family",
        "source_type",
        "source_snapshot_at",
        "source_artifact",
        "source_record_id",
        "source_license",
        "source_sha256",
        "licenses",
        "lineage_policy",
    }
    missing = sorted(required - row.keys())
    if missing:
        raise BenchmarkError(
            f"{path}:{line_no}: missing schema-4 source fields: {', '.join(missing)}"
        )
    source_id = _required_text(path, line_no, "truth_source_id", row["truth_source_id"])
    source = V4_SOURCE_CATALOG.get(source_id)
    if source is None:
        raise BenchmarkError(f"{path}:{line_no}: unsupported truth_source_id {source_id!r}")
    expected = {
        "truth_source_family": source["family"],
        "source_release": source["source_release"],
        "source_theme": source["theme"],
        "source_type": source["type"],
        "source_snapshot_at": source["snapshot_at"],
        "source_sha256": source["retained_input"]["sha256"],
    }
    for field, expected_value in expected.items():
        if row.get(field) != expected_value:
            raise BenchmarkError(
                f"{path}:{line_no}: {field} does not match fixed source {source_id}: "
                f"expected {expected_value!r}, got {row.get(field)!r}"
            )
    if row["country"] not in V4_SOURCE_COUNTRIES[source_id]:
        raise BenchmarkError(
            f"{path}:{line_no}: source {source_id} is not pinned for country {row['country']}"
        )
    _required_text(path, line_no, "source_record_id", row["source_record_id"])
    _validate_utc_timestamp(
        row["source_snapshot_at"], f"{path}:{line_no}: source_snapshot_at"
    )
    artifact = _validate_fixed_artifact(
        row["source_artifact"],
        f"{path}:{line_no}: source_artifact",
        source["retained_input"],
    )
    if row["source_sha256"] != artifact["sha256"]:
        raise BenchmarkError(f"{path}:{line_no}: source_sha256 must equal source_artifact.sha256")
    if row["source_license"] != row["license"]:
        raise BenchmarkError(f"{path}:{line_no}: source_license must equal row license")
    if row["licenses"] != [row["license"]]:
        raise BenchmarkError(f"{path}:{line_no}: licenses must contain exactly the row license")
    if row["lineage_policy"] != LINEAGE_POLICY:
        raise BenchmarkError(f"{path}:{line_no}: lineage_policy does not match the benchmark")
    provenance = row.get("coordinate_provenance")
    if not isinstance(provenance, dict):
        raise BenchmarkError(f"{path}:{line_no}: coordinate_provenance must be an object")
    provenance_expected = {
        "source_family": source["family"],
        "source_snapshot_at": source["snapshot_at"],
        "snapshot_logical_name": artifact["logical_name"],
        "snapshot_sha256": artifact["sha256"],
    }
    for field, expected_value in provenance_expected.items():
        if provenance.get(field) != expected_value:
            raise BenchmarkError(
                f"{path}:{line_no}: coordinate_provenance.{field} does not match "
                f"fixed source {source_id}"
            )
    if source["family"] == OSM_SOURCE_FAMILY:
        if row["source_license"] != source["license"]:
            raise BenchmarkError(
                f"{path}:{line_no}: source_license does not match fixed source {source_id}"
            )
        if row["source_url"] != source["public_uri"]:
            raise BenchmarkError(
                f"{path}:{line_no}: source_url does not match fixed source {source_id}"
            )
        pbf = source["pbf"]
        osm_expected = {
            "source_name": source["dataset"],
            "coordinate_method": "node_location",
            "object_type": "node",
            "record_id": row["source_record_id"],
            "replication_base_url": pbf["replication_base_url"],
            "replication_sequence": pbf["replication_sequence"],
            "attribution_url": source["copyright_url"],
            "license": source["license"],
        }
        for field, expected_value in osm_expected.items():
            if provenance.get(field) != expected_value:
                raise BenchmarkError(
                    f"{path}:{line_no}: coordinate_provenance.{field} does not match "
                    f"fixed source {source_id}"
                )
        object_id = provenance.get("object_id")
        if (
            type(object_id) is not int
            or object_id <= 0
            or row["source_record_id"] != f"node/{object_id}"
        ):
            raise BenchmarkError(
                f"{path}:{line_no}: OSM object_id does not bind source_record_id"
            )
        object_url = f"https://www.openstreetmap.org/{row['source_record_id']}"
        if provenance.get("source_url") != object_url or provenance.get("evidence_url") != object_url:
            raise BenchmarkError(
                f"{path}:{line_no}: OSM coordinate object URL does not bind source_record_id"
            )
    else:
        if row["retrieved_at"] != source["snapshot_at"]:
            raise BenchmarkError(
                f"{path}:{line_no}: Overture retrieved_at does not match acquisition snapshot"
            )
        if row["source_url"] != provenance.get("source_url"):
            raise BenchmarkError(
                f"{path}:{line_no}: Overture source_url must equal coordinate source_url"
            )
        if provenance.get("acquisition_manifest_sha256") != source["acquisition_manifest"]["sha256"]:
            raise BenchmarkError(
                f"{path}:{line_no}: coordinate acquisition_manifest_sha256 does not match fixed source"
            )
        coordinate_datasets = row.get("coordinate_source_dataset")
        coordinate_records = row.get("coordinate_source_records")
        if coordinate_datasets != [provenance.get("source_name")]:
            raise BenchmarkError(
                f"{path}:{line_no}: coordinate_source_dataset does not bind coordinate provenance"
            )
        if (
            not isinstance(coordinate_records, list)
            or len(coordinate_records) != 1
            or not isinstance(coordinate_records[0], dict)
        ):
            raise BenchmarkError(
                f"{path}:{line_no}: coordinate_source_records must contain one source record"
            )
        coordinate_record = coordinate_records[0]
        coordinate_record_expected = {
            "dataset": provenance.get("source_name"),
            "record_id": provenance.get("record_id"),
            "license": row["license"],
        }
        for field, expected_value in coordinate_record_expected.items():
            if coordinate_record.get(field) != expected_value:
                raise BenchmarkError(
                    f"{path}:{line_no}: coordinate_source_records[0].{field} does not "
                    "bind coordinate provenance"
                )


def _lineage_counts(rows: list[dict]) -> tuple[dict[str, int], dict[str, int], dict[str, dict[str, int]]]:
    by_country = {country: 0 for country in COUNTRIES}
    by_lineage = {lineage: 0 for lineage in LINEAGE_CLASSES}
    by_country_and_lineage = {
        country: {lineage: 0 for lineage in LINEAGE_CLASSES}
        for country in COUNTRIES
    }
    for row in rows:
        country = row["country"]
        lineage = row["lineage_class"]
        by_country[country] += 1
        by_lineage[lineage] += 1
        by_country_and_lineage[country][lineage] += 1
    return by_country, by_lineage, by_country_and_lineage


def _validate_truth_manifest(path: pathlib.Path, rows: list[dict]) -> dict:
    manifest_path = _truth_manifest_path(path)
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise BenchmarkError(f"truth manifest is missing or invalid: {manifest_path}: {exc}") from exc
    if not isinstance(manifest, dict):
        raise BenchmarkError(f"truth manifest must be a JSON object: {manifest_path}")
    corpus_schema = _corpus_schema(rows)
    expected = {
        "schema": corpus_schema,
        "sha256": _sha256(path),
        "rows": len(rows),
        "lineage_policy": LINEAGE_POLICY,
    }
    for key, value in expected.items():
        if manifest.get(key) != value:
            raise BenchmarkError(
                f"truth manifest mismatch for {key}: expected {value!r}, got {manifest.get(key)!r}"
            )
    by_country, by_lineage, by_country_and_lineage = _lineage_counts(rows)
    if manifest.get("rows_by_country") != by_country:
        raise BenchmarkError("truth manifest rows_by_country does not match the corpus")
    if manifest.get("rows_by_lineage") != by_lineage:
        raise BenchmarkError("truth manifest rows_by_lineage does not match the corpus")
    if manifest.get("rows_by_country_and_lineage") != by_country_and_lineage:
        raise BenchmarkError(
            "truth manifest rows_by_country_and_lineage does not match the corpus"
        )
    licenses = sorted({row["license"] for row in rows})
    if manifest.get("licenses") != licenses:
        raise BenchmarkError("truth manifest licenses do not match the corpus")
    if corpus_schema == MULTI_SOURCE_TRUTH_SCHEMA:
        if "retrieved_at" in manifest:
            raise BenchmarkError(
                "schema-4 truth manifest must use assembled_at, not a single retrieved_at"
            )
        assembled_at = _validate_utc_timestamp(
            manifest.get("assembled_at"), "truth manifest assembled_at"
        )
        source_catalog = _validate_v4_source_catalog(
            manifest_path, manifest.get("source_catalog")
        )
        _manifest_recipe(manifest_path, manifest)
        source_ids = tuple(sorted(source_catalog))
        by_source, by_country_and_source, by_country_source_lineage = _source_counts(
            rows, source_ids
        )
        if by_source != V4_ROWS_BY_SOURCE:
            raise BenchmarkError("schema-4 corpus does not match fixed rows_by_source quotas")
        if by_country_and_source != V4_ROWS_BY_COUNTRY_AND_SOURCE:
            raise BenchmarkError(
                "schema-4 corpus does not match fixed rows_by_country_and_source quotas"
            )
        if manifest.get("rows_by_source") != by_source:
            raise BenchmarkError("truth manifest rows_by_source does not match the corpus")
        if manifest.get("rows_by_country_and_source") != by_country_and_source:
            raise BenchmarkError(
                "truth manifest rows_by_country_and_source does not match the corpus"
            )
        if (
            manifest.get("rows_by_country_source_and_lineage")
            != by_country_source_lineage
        ):
            raise BenchmarkError(
                "truth manifest rows_by_country_source_and_lineage does not match the corpus"
            )
        unused = sorted(source_id for source_id, count in by_source.items() if count == 0)
        if unused:
            raise BenchmarkError(
                "truth manifest source_catalog contains unused fixed sources: "
                + ", ".join(unused)
            )
        for line_no, row in enumerate(rows, 1):
            source_id = row["truth_source_id"]
            expected_retrieved_at = (
                assembled_at
                if source_catalog[source_id]["family"] == OSM_SOURCE_FAMILY
                else source_catalog[source_id]["snapshot_at"]
            )
            if row["retrieved_at"] != expected_retrieved_at:
                raise BenchmarkError(
                    f"{path}:{line_no}: retrieved_at does not bind schema-4 source assembly"
                )
        return manifest
    retrieved_at = _validate_utc_timestamp(
        manifest.get("retrieved_at"), "truth manifest retrieved_at"
    )
    source_details = _manifest_source_details(manifest_path, manifest)
    _manifest_recipe(manifest_path, manifest)
    if source_details["retrieved_at"] != retrieved_at:
        raise BenchmarkError(
            "truth manifest source_details.retrieved_at does not match retrieved_at"
        )
    row_retrieved_at = {row["retrieved_at"] for row in rows}
    if row_retrieved_at != {retrieved_at}:
        raise BenchmarkError(
            "truth manifest retrieved_at does not match every corpus row"
        )
    row_releases = {row["source_release"] for row in rows}
    if row_releases != {source_details["source_release"]}:
        raise BenchmarkError(
            "truth manifest source_details.source_release does not match every corpus row"
        )
    row_themes = {row["source_theme"] for row in rows}
    if row_themes != {source_details["theme"]}:
        raise BenchmarkError(
            "truth manifest source_details.theme does not match every corpus row"
        )
    return manifest


def _profile_normalize(value: object) -> str:
    folded = unicodedata.normalize("NFKC", str(value).casefold())
    return " ".join(
        "".join(
            character if character.isalnum() else " "
            for character in folded
        ).split()
    )


def _profile_selected_ids_sha256(rows: list[dict]) -> str:
    record_ids = sorted(str(row["record_id"]) for row in rows)
    payload = ("\n".join(record_ids) + ("\n" if record_ids else "")).encode()
    return hashlib.sha256(payload).hexdigest()


def _profile_street_name(row: dict) -> str:
    street_address = " ".join(str(row.get("street_address", "")).split())
    without_number = re.sub(
        r"\b\d+[\w/-]*\b", " ", street_address, flags=re.UNICODE
    )
    return " ".join(without_number.split()) or street_address


def _foursquare_profile_verification(path: pathlib.Path, rows: list[dict]) -> dict:
    if _profile_selected_ids_sha256(rows) != FOURSQUARE_OUTSIDE_230_SELECTED_RECORD_IDS_SHA256:
        raise BenchmarkError(
            f"{FOURSQUARE_OUTSIDE_230_PROFILE} rows do not match the approved witness selection"
        )

    dedupe_keys: dict[str, list[object]] = {
        "record_id": [str(row["record_id"]) for row in rows],
        "normalized_query": [
            "".join(character for character in _profile_normalize(row["query"]) if character.isalnum())
            for row in rows
        ],
        "coordinate_5dp": [
            (round(float(row["lat"]), 5), round(float(row["lon"]), 5))
            for row in rows
        ],
    }
    deduplication: dict[str, dict[str, int]] = {}
    for name, values in dedupe_keys.items():
        counts = collections.Counter(values)
        duplicate_groups = [count for count in counts.values() if count > 1]
        deduplication[name] = {
            "distinct": len(counts),
            "duplicate_groups": len(duplicate_groups),
            "rows_in_duplicate_groups": sum(duplicate_groups),
        }
        if duplicate_groups:
            raise BenchmarkError(
                f"{path}: {FOURSQUARE_OUTSIDE_230_PROFILE} violates global {name} deduplication"
            )

    countries: dict[str, dict] = {}
    for country in COUNTRIES:
        scoped = [row for row in rows if row["country"] == country]
        dimension_values: dict[str, list[str]] = {
            dimension: [] for dimension in FOURSQUARE_OUTSIDE_230_CAPS
        }
        for line_no, row in enumerate(rows, 1):
            if row["country"] != country:
                continue
            derived_street = _profile_street_name(row)
            if row.get("street_name") != derived_street:
                raise BenchmarkError(
                    f"{path}:{line_no}: street_name does not match the profile cap key"
                )
            category = row.get("category", "")
            network = row.get("network", "")
            municipality = _profile_normalize(row["municipality"])
            street = _profile_normalize(derived_street)
            values = {
                "municipality": municipality,
                "street": f"{municipality}|{street}" if street else "",
                "cell_0_01_degree": (
                    f"{math.floor(float(row['lat']) * 100)}:"
                    f"{math.floor(float(row['lon']) * 100)}"
                ),
                "category": _profile_normalize(category),
                "network": _profile_normalize(network),
            }
            for dimension, value in values.items():
                if value:
                    dimension_values[dimension].append(value)

        maxima: dict[str, int] = {}
        for dimension, cap in FOURSQUARE_OUTSIDE_230_CAPS.items():
            maximum = max(
                collections.Counter(dimension_values[dimension]).values(),
                default=0,
            )
            maxima[dimension] = maximum
            if maximum > cap:
                raise BenchmarkError(
                    f"{path}: {country} {dimension} maximum {maximum} exceeds profile cap {cap}"
                )
        countries[country] = {
            "rows": len(scoped),
            "maximum_observed_group_use": maxima,
            "caps": dict(FOURSQUARE_OUTSIDE_230_CAPS),
            "status": "verified",
        }

    return {
        "scope": "global deduplication; per-country concentration caps",
        "deduplication": deduplication,
        "countries": countries,
    }


def _validate_benchmark_profile_manifest(
    manifest: dict,
    benchmark_profile: str | None,
) -> None:
    if benchmark_profile is None:
        return
    if benchmark_profile != FOURSQUARE_OUTSIDE_230_PROFILE:
        raise BenchmarkError(f"unsupported benchmark profile {benchmark_profile!r}")
    if manifest.get("benchmark_profile") != benchmark_profile:
        raise BenchmarkError(
            f"truth manifest must bind benchmark_profile={benchmark_profile!r}"
        )
    if manifest.get("sha256") != FOURSQUARE_OUTSIDE_230_CORPUS_SHA256:
        raise BenchmarkError(
            f"truth manifest must bind the approved {benchmark_profile} corpus SHA-256"
        )
    thresholds = manifest.get("benchmark_thresholds")
    if (
        not isinstance(thresholds, dict)
        or thresholds.get("required_rows_by_country") != FOURSQUARE_OUTSIDE_230_COUNTS
        or thresholds.get("required_unknown_lineage") != 0
    ):
        raise BenchmarkError(
            f"truth manifest has invalid {benchmark_profile} threshold binding"
        )
    artifacts = manifest.get("input_artifacts")
    expected_hashes = {
        "exact_cap_witness": FOURSQUARE_OUTSIDE_230_WITNESS_SHA256,
        "overture_acquisition": OVERTURE_RAW_SHA256,
        "overture_acquisition_manifest": OVERTURE_ACQUISITION_MANIFEST_SHA256,
    }
    if not isinstance(artifacts, dict) or any(
        not isinstance(artifacts.get(name), dict)
        or artifacts[name].get("sha256") != expected_hash
        for name, expected_hash in expected_hashes.items()
    ):
        raise BenchmarkError(
            f"truth manifest has invalid {benchmark_profile} input SHA-256 binding"
        )
    selection = manifest.get("selection")
    diagnostics = manifest.get("diagnostics")
    if (
        not isinstance(selection, dict)
        or selection.get("provider") != "Foursquare"
        or selection.get("lineage_class") != "outside_chain"
        or selection.get("rows_by_country") != FOURSQUARE_OUTSIDE_230_COUNTS
        or selection.get("diversity_caps") != FOURSQUARE_OUTSIDE_230_CAPS
        or selection.get("selected_record_ids_sha256")
        != FOURSQUARE_OUTSIDE_230_SELECTED_RECORD_IDS_SHA256
        or manifest.get("coordinate_provider_counts") != {"Foursquare": 230}
        or not isinstance(diagnostics, dict)
        or not isinstance(diagnostics.get("path"), str)
        or not re.fullmatch(r"[0-9a-f]{64}", str(diagnostics.get("sha256", "")))
    ):
        raise BenchmarkError(
            f"truth manifest has invalid {benchmark_profile} selection binding"
        )
    source_details = _validate_primary_measurement_source(manifest)
    if source_details.get("license") != "Apache-2.0":
        raise BenchmarkError(
            f"truth manifest has invalid {benchmark_profile} source license"
        )


def _validate_benchmark_profile_diagnostics(
    truth_path: pathlib.Path,
    rows: list[dict],
    manifest: dict,
    benchmark_profile: str | None,
) -> dict | None:
    if benchmark_profile is None:
        return None
    diagnostics_reference = manifest["diagnostics"]
    expected_name = truth_path.name + ".diagnostics.json"
    if diagnostics_reference["path"] != expected_name:
        raise BenchmarkError(
            f"truth manifest diagnostics path must be adjacent {expected_name!r}"
        )
    diagnostics_path = truth_path.with_name(expected_name)
    capture = _capture_regular_nofollow(
        diagnostics_path,
        "truth diagnostics",
        single_link=True,
    )
    if capture["sha256"] != diagnostics_reference["sha256"]:
        raise BenchmarkError("truth manifest diagnostics SHA-256 does not match sidecar")
    try:
        diagnostics = json.loads(capture["_path"].read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise BenchmarkError(f"truth diagnostics are invalid: {exc}") from exc
    _verify_capture(capture)
    expected_artifact_hashes = {
        "exact_cap_witness": FOURSQUARE_OUTSIDE_230_WITNESS_SHA256,
        "overture_acquisition": OVERTURE_RAW_SHA256,
        "overture_acquisition_manifest": OVERTURE_ACQUISITION_MANIFEST_SHA256,
    }
    artifacts = diagnostics.get("input_artifacts")
    protected = diagnostics.get("protected_policy")
    if (
        diagnostics.get("schema") != 1
        or diagnostics.get("kind") != "foursquare_outside_selection_diagnostics"
        or diagnostics.get("status") != "verified"
        or diagnostics.get("selected_record_ids_sha256")
        != FOURSQUARE_OUTSIDE_230_SELECTED_RECORD_IDS_SHA256
        or not isinstance(artifacts, dict)
        or any(
            not isinstance(artifacts.get(name), dict)
            or artifacts[name].get("sha256") != expected_hash
            for name, expected_hash in expected_artifact_hashes.items()
        )
        or not isinstance(protected, dict)
        or protected
        != {
            "default_minimum": DEFAULT_MINIMUM,
            "default_unknown_minimum": DEFAULT_UNKNOWN_MINIMUM,
            "markers_sha256": FOURSQUARE_OUTSIDE_230_MARKERS_SHA256,
            "policy_changed": False,
        }
        or diagnostics.get("selection_verification")
        != _foursquare_profile_verification(truth_path, rows)
    ):
        raise BenchmarkError(
            f"truth diagnostics do not prove {FOURSQUARE_OUTSIDE_230_PROFILE}"
        )
    return capture


def _validate_primary_measurement_source(manifest: dict) -> dict:
    if manifest.get("schema") == MULTI_SOURCE_TRUTH_SCHEMA:
        catalog = manifest.get("source_catalog")
        if not isinstance(catalog, dict):
            raise BenchmarkError("schema-4 primary measurement requires source_catalog")
        entry = catalog.get(OVERTURE_SOURCE_ID)
        _require_pinned_subset(
            entry,
            V4_SOURCE_CATALOG[OVERTURE_SOURCE_ID],
            f"truth manifest source_catalog.{OVERTURE_SOURCE_ID}",
        )
        return {
            "dataset": entry["dataset"],
            "theme": entry["theme"],
            "type": entry["type"],
            "source_release": entry["source_release"],
            "uri": entry["public_uri"],
            "retrieved_at": entry["snapshot_at"],
            "license": entry["license"],
        }
    details = manifest["source_details"]
    expected = {
        "dataset": OVERTURE_DATASET,
        "theme": OVERTURE_THEME,
        "type": OVERTURE_TYPE,
        "source_release": OVERTURE_RELEASE,
        "uri": OVERTURE_S3,
    }
    mismatches = [
        f"{field}={details.get(field)!r} (expected {value!r})"
        for field, value in expected.items()
        if details.get(field) != value
    ]
    if mismatches:
        raise BenchmarkError(
            "primary measurement requires the pinned official Overture Maps Places "
            "source; " + "; ".join(mismatches)
        )
    return details


def _atomic_jsonl(path: pathlib.Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, raw_tmp = tempfile.mkstemp(prefix=path.name + ".", suffix=".part", dir=path.parent)
    tmp = pathlib.Path(raw_tmp)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            for row in rows:
                handle.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink()


def _atomic_json(path: pathlib.Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, raw_tmp = tempfile.mkstemp(prefix=path.name + ".", suffix=".part", dir=path.parent)
    tmp = pathlib.Path(raw_tmp)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink()


def _load_jsonl(path: pathlib.Path) -> list[dict]:
    rows: list[dict] = []
    try:
        with path.open(encoding="utf-8") as handle:
            for line_no, line in enumerate(handle, 1):
                if not line.strip():
                    raise BenchmarkError(f"{path}:{line_no}: blank rows are not allowed")
                value = json.loads(line)
                if not isinstance(value, dict):
                    raise BenchmarkError(f"{path}:{line_no}: expected a JSON object")
                rows.append(value)
    except (OSError, json.JSONDecodeError) as exc:
        raise BenchmarkError(f"cannot read {path}: {exc}") from exc
    return rows


def _load_jsonl_stream(handle, path: pathlib.Path) -> list[dict]:
    rows: list[dict] = []
    try:
        handle.seek(0)
        for line_no, line in enumerate(handle, 1):
            if not line.strip():
                raise BenchmarkError(f"{path}:{line_no}: blank rows are not allowed")
            value = json.loads(line)
            if not isinstance(value, dict):
                raise BenchmarkError(f"{path}:{line_no}: expected a JSON object")
            rows.append(value)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BenchmarkError(f"cannot read {path}: {exc}") from exc
    return rows


def _load_jsonl_regular(
    path: pathlib.Path,
    label: str,
    *,
    single_link: bool = False,
) -> list[dict]:
    _require_regular_nofollow(path, label, single_link=single_link)
    fd = _open_regular_nofollow(
        path, os.O_RDONLY, label, single_link=single_link
    )
    with os.fdopen(fd, "r", encoding="utf-8") as handle:
        return _load_jsonl_stream(handle, path)


def _load_jsonl_regular_snapshot(
    path: pathlib.Path,
    label: str,
    *,
    single_link: bool,
) -> tuple[list[dict], str]:
    """Parse and hash the exact bytes read from one verified regular-file FD."""
    _require_regular_nofollow(path, label, single_link=single_link)
    fd = _open_regular_nofollow(
        path,
        os.O_RDONLY,
        label,
        single_link=single_link,
    )
    with os.fdopen(fd, "rb") as handle:
        before = os.fstat(handle.fileno())
        raw = handle.read()
        after = os.fstat(handle.fileno())
    stable_fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns")
    if any(getattr(before, field) != getattr(after, field) for field in stable_fields):
        raise BenchmarkError(f"{label} changed while its bytes were being read: {path}")
    if not _same_file_identity(path, after):
        raise BenchmarkError(f"{label} path changed while its bytes were being read: {path}")
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise BenchmarkError(f"cannot read {path}: {exc}") from exc
    rows = _load_jsonl_stream(io.StringIO(text), path)
    return rows, hashlib.sha256(raw).hexdigest()


def _sha256_regular(path: pathlib.Path, label: str, *, single_link: bool = False) -> str:
    _require_regular_nofollow(path, label, single_link=single_link)
    fd = _open_regular_nofollow(
        path, os.O_RDONLY, label, single_link=single_link
    )
    digest = hashlib.sha256()
    with os.fdopen(fd, "rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _address_key(value: str) -> str:
    return "".join(character.casefold() for character in value if character.isalnum())


def _inside_product_extent(country: str, lat: float, lon: float) -> bool:
    west, south, east, north = COUNTRIES[country]["bounds"]
    return west <= lon <= east and south <= lat <= north


def _clean_text(value: object) -> str:
    return " ".join(value.split()) if isinstance(value, str) else ""


def _compose_query(row: dict) -> str:
    locality = " ".join(
        part
        for part in (
            _clean_text(row.get("postcode")),
            _clean_text(row.get("municipality")),
        )
        if part
    )
    return ", ".join(
        part
        for part in (
            _clean_text(row.get("street_address")),
            locality,
            COUNTRIES[row["country"]]["name"],
        )
        if part
    )


def _canonical_wikidata_url(raw_url: str) -> str:
    parsed = urllib.parse.urlsplit(raw_url)
    if parsed.scheme not in {"http", "https"} or parsed.hostname != "www.wikidata.org":
        raise BenchmarkError(f"invalid Wikidata URL: {raw_url!r}")
    return urllib.parse.urlunsplit(("https", "www.wikidata.org", parsed.path, "", ""))


def _binding_text(binding: dict, name: str) -> str:
    try:
        return _clean_text(binding[name]["value"])
    except (KeyError, TypeError):
        return ""


def _statement_provenance(binding: dict, prefix: str) -> dict:
    statement_url = _canonical_wikidata_url(_binding_text(binding, f"{prefix}Statement"))
    reference_url = _binding_text(binding, f"{prefix}ReferenceUrl")
    if reference_url:
        parsed = urllib.parse.urlsplit(reference_url)
        if parsed.scheme not in {"http", "https"} or not parsed.hostname:
            raise BenchmarkError(f"invalid {prefix} reference URL: {reference_url!r}")
    stated_in_url = _binding_text(binding, f"{prefix}StatedIn")
    stated_in_qid = stated_in_url.rsplit("/", 1)[-1] if stated_in_url else ""
    if stated_in_url:
        stated_in_url = _canonical_wikidata_url(stated_in_url)
    return {
        "statement_url": statement_url,
        "reference_url": reference_url or None,
        "stated_in_qid": stated_in_qid or None,
        "stated_in_url": stated_in_url or None,
        "stated_in_label": _binding_text(binding, f"{prefix}StatedInLabel") or None,
    }


def _normalized_tokens(value: object) -> tuple[str, ...]:
    if not isinstance(value, str):
        return ()
    normalized = unicodedata.normalize("NFKC", value).casefold()
    return tuple(
        "".join(character if character.isalnum() else " " for character in normalized)
        .split()
    )


def _contains_normalized_token_sequence(value: object, phrase: object) -> bool:
    tokens = _normalized_tokens(value)
    marker = _normalized_tokens(phrase)
    if not marker or len(marker) > len(tokens):
        return False
    width = len(marker)
    return any(tokens[index:index + width] == marker for index in range(len(tokens) - width + 1))


def _exact_url_hostname(value: object) -> str | None:
    if not isinstance(value, str):
        return None
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme.lower() not in {"http", "https"} or not parsed.hostname:
        return None
    return parsed.hostname.casefold().rstrip(".")


def _common_lineage_marker(country: str, provenance: dict) -> str | None:
    values = tuple(
        part
        for part in (
            provenance.get("reference_url"),
            provenance.get("stated_in_qid"),
            provenance.get("stated_in_url"),
            provenance.get("stated_in_label"),
        )
        if isinstance(part, str) and part.strip()
    )
    hostnames = {
        hostname
        for hostname in (_exact_url_hostname(value) for value in values)
        if hostname is not None
    }
    labels = tuple(value for value in values if _exact_url_hostname(value) is None)
    for canonical, phrases, exact_hostnames in COUNTRY_COMMON_LINEAGE[country]:
        phrase_match = any(
            _contains_normalized_token_sequence(label, phrase)
            for label in labels
            for phrase in phrases
        )
        hostname_match = any(
            hostname.casefold().rstrip(".") in hostnames
            for hostname in exact_hostnames
        )
        if phrase_match or hostname_match:
            return canonical
    return None


def _wikidata_coordinate_provenance(
    country: str,
    record_id: str,
    retrieved_at: str,
    statement: dict,
) -> tuple[str, dict]:
    has_upstream = bool(statement.get("reference_url") or statement.get("stated_in_qid"))
    common_ancestor = _common_lineage_marker(country, statement) if has_upstream else None
    if common_ancestor:
        lineage_class = "common_upstream"
    else:
        # A reference proves that a source was named, not that it sits outside
        # the GridPin input lineage.  Only an independently curated generic
        # corpus may assert outside_chain with explicit evidence.
        lineage_class = "unknown_lineage"
    return lineage_class, {
        "source_name": "Wikidata",
        "source_url": f"https://www.wikidata.org/wiki/{record_id}",
        "record_id": record_id,
        "retrieved_at": retrieved_at,
        "license": WIKIDATA_LICENSE,
        "common_ancestor": common_ancestor,
        "evidence_url": statement["statement_url"] if has_upstream else None,
        "upstream_reference_url": statement.get("reference_url"),
        "upstream_stated_in_qid": statement.get("stated_in_qid"),
        "upstream_stated_in_url": statement.get("stated_in_url"),
        "upstream_stated_in_label": statement.get("stated_in_label"),
        "same_export_as_indexed_sheet": False,
    }


def _absolute_public_url(path: pathlib.Path, line_no: int, name: str, value: object) -> str:
    if not isinstance(value, str) or not value.strip():
        raise BenchmarkError(f"{path}:{line_no}: {name} must be a non-empty URL")
    parsed = urllib.parse.urlsplit(value)
    if (
        parsed.scheme.lower() not in {"http", "https"}
        or not parsed.hostname
        or parsed.username
        or parsed.password
    ):
        raise BenchmarkError(f"{path}:{line_no}: {name} must be an absolute public HTTP(S) URL")
    return value


def _required_text(path: pathlib.Path, line_no: int, name: str, value: object) -> str:
    if not isinstance(value, str) or not _clean_text(value):
        raise BenchmarkError(f"{path}:{line_no}: {name} must be non-empty text")
    return value


def _validate_coordinate_provenance(
    path: pathlib.Path,
    line_no: int,
    lineage_class: str,
    value: object,
) -> None:
    if not isinstance(value, dict):
        raise BenchmarkError(f"{path}:{line_no}: coordinate_provenance must be an object")
    required = {
        "source_name",
        "source_url",
        "record_id",
        "retrieved_at",
        "license",
        "same_export_as_indexed_sheet",
    }
    missing = sorted(required - value.keys())
    if missing:
        raise BenchmarkError(
            f"{path}:{line_no}: coordinate_provenance missing fields: {', '.join(missing)}"
        )
    _required_text(path, line_no, "coordinate source_name", value["source_name"])
    _absolute_public_url(path, line_no, "coordinate source_url", value["source_url"])
    _required_text(path, line_no, "coordinate record_id", value["record_id"])
    _validate_utc_timestamp(
        value["retrieved_at"], f"{path}:{line_no}: coordinate retrieved_at"
    )
    _required_text(path, line_no, "coordinate license", value["license"])

    evidence_url = value.get("evidence_url")
    if evidence_url is not None:
        _absolute_public_url(path, line_no, "coordinate evidence_url", evidence_url)
    common_ancestor = value.get("common_ancestor")
    if lineage_class == "common_upstream":
        _required_text(path, line_no, "coordinate common_ancestor", common_ancestor)
    elif common_ancestor is not None:
        raise BenchmarkError(
            f"{path}:{line_no}: common_ancestor requires lineage_class=common_upstream"
        )
    if lineage_class == "outside_chain" and evidence_url is None:
        raise BenchmarkError(
            f"{path}:{line_no}: outside_chain coordinate provenance needs evidence_url"
        )
    if value["same_export_as_indexed_sheet"] is not False:
        raise BenchmarkError(
            f"{path}:{line_no}: same_export_as_indexed_sheet must be the boolean false"
        )


def validate_truth(
    path: pathlib.Path,
    minimum: int = DEFAULT_MINIMUM,
    unknown_minimum: int = DEFAULT_UNKNOWN_MINIMUM,
    benchmark_profile: str | None = None,
) -> list[dict]:
    if benchmark_profile is None:
        if minimum < DEFAULT_MINIMUM:
            raise BenchmarkError(f"minimum must be at least {DEFAULT_MINIMUM}")
        if unknown_minimum < DEFAULT_UNKNOWN_MINIMUM:
            raise BenchmarkError(
                f"unknown lineage minimum must be at least {DEFAULT_UNKNOWN_MINIMUM}"
            )
    elif benchmark_profile == FOURSQUARE_OUTSIDE_230_PROFILE:
        if minimum != FOURSQUARE_OUTSIDE_230_MINIMUM or unknown_minimum != 0:
            raise BenchmarkError(
                f"{FOURSQUARE_OUTSIDE_230_PROFILE} requires exactly "
                f"--minimum {FOURSQUARE_OUTSIDE_230_MINIMUM} --unknown-minimum 0"
            )
    else:
        raise BenchmarkError(f"unsupported benchmark profile {benchmark_profile!r}")
    if unknown_minimum > minimum:
        raise BenchmarkError("unknown lineage minimum cannot exceed the total minimum")
    rows = _load_jsonl(path)
    corpus_schema = _corpus_schema(rows)
    counts: collections.Counter[str] = collections.Counter()
    unknown_counts: collections.Counter[str] = collections.Counter()
    identities: set[tuple[str, str]] = set()
    address_keys: set[tuple[str, str]] = set()
    coordinate_keys: set[tuple[str, float, float]] = set()
    for line_no, row in enumerate(rows, 1):
        if not isinstance(row, dict):
            raise BenchmarkError(f"{path}:{line_no}: truth row must be a JSON object")
        required = {
            "schema",
            "country",
            "query",
            "street_address",
            "postcode",
            "municipality",
            "lat",
            "lon",
            "record_id",
            "source_url",
            "source_release",
            "source_theme",
            "coordinate_provenance",
            "license",
            "retrieved_at",
            "lineage_class",
        }
        missing = sorted(required - row.keys())
        if missing:
            raise BenchmarkError(f"{path}:{line_no}: missing fields: {', '.join(missing)}")
        if type(row["schema"]) is not int or row["schema"] != corpus_schema:
            raise BenchmarkError(
                f"{path}:{line_no}: schema must be the integer {corpus_schema}"
            )
        country = row["country"]
        if country not in COUNTRIES:
            raise BenchmarkError(f"{path}:{line_no}: unsupported country {country!r}")
        for field in ("street_address", "municipality"):
            if not isinstance(row[field], str) or not _clean_text(row[field]):
                raise BenchmarkError(f"{path}:{line_no}: {field} must be non-empty text")
        if not isinstance(row["postcode"], str):
            raise BenchmarkError(f"{path}:{line_no}: postcode must be text")
        if not any(character.isalpha() for character in row["street_address"]) or not any(
            character.isdigit() for character in row["street_address"]
        ):
            raise BenchmarkError(f"{path}:{line_no}: street_address must contain text and a house number")
        if row["postcode"] and not any(character.isdigit() for character in row["postcode"]):
            raise BenchmarkError(f"{path}:{line_no}: non-empty postcode must contain a digit")
        if not any(character.isalpha() for character in row["municipality"]):
            raise BenchmarkError(f"{path}:{line_no}: municipality must contain alphabetic text")
        if row["query"] != _compose_query(row):
            raise BenchmarkError(
                f"{path}:{line_no}: query must include street address, municipality and country"
            )
        try:
            lat, lon = float(row["lat"]), float(row["lon"])
        except (OverflowError, TypeError, ValueError) as exc:
            raise BenchmarkError(f"{path}:{line_no}: invalid coordinates") from exc
        if not (math.isfinite(lat) and math.isfinite(lon)):
            raise BenchmarkError(f"{path}:{line_no}: coordinates must be finite")
        if not (-90.0 <= lat <= 90.0 and -180.0 <= lon <= 180.0):
            raise BenchmarkError(f"{path}:{line_no}: coordinates are out of range")
        if not _inside_product_extent(country, lat, lon):
            raise BenchmarkError(f"{path}:{line_no}: coordinates are outside the {country} product extent")
        record_id = row["record_id"]
        _required_text(path, line_no, "record_id", record_id)
        _absolute_public_url(path, line_no, "source_url", row["source_url"])
        _required_text(path, line_no, "source_release", row["source_release"])
        _required_text(path, line_no, "source_theme", row["source_theme"])
        _required_text(path, line_no, "license", row["license"])
        _validate_utc_timestamp(row["retrieved_at"], f"{path}:{line_no}: retrieved_at")
        lineage_class = row["lineage_class"]
        if lineage_class not in LINEAGE_CLASSES:
            raise BenchmarkError(
                f"{path}:{line_no}: lineage_class must be exactly one of "
                + ", ".join(LINEAGE_CLASSES)
            )
        _validate_coordinate_provenance(
            path,
            line_no,
            lineage_class,
            row["coordinate_provenance"],
        )
        coordinate_provenance = row["coordinate_provenance"]
        if coordinate_provenance["retrieved_at"] != row["retrieved_at"]:
            raise BenchmarkError(
                f"{path}:{line_no}: coordinate retrieved_at must equal row retrieved_at"
            )
        if coordinate_provenance["license"] != row["license"]:
            raise BenchmarkError(
                f"{path}:{line_no}: coordinate license must equal row license"
            )
        if corpus_schema == MULTI_SOURCE_TRUTH_SCHEMA:
            _validate_v4_row_source(path, line_no, row)
        for optional_field in ("category",):
            if optional_field in row and row[optional_field] is not None:
                _required_text(path, line_no, optional_field, row[optional_field])
        if "confidence" in row and row["confidence"] is not None:
            try:
                confidence = float(row["confidence"])
            except (OverflowError, TypeError, ValueError) as exc:
                raise BenchmarkError(f"{path}:{line_no}: confidence must be numeric") from exc
            if not math.isfinite(confidence) or not 0.0 <= confidence <= 1.0:
                raise BenchmarkError(f"{path}:{line_no}: confidence must be between 0 and 1")
        identity = (country, record_id)
        if identity in identities:
            raise BenchmarkError(f"{path}:{line_no}: duplicate country/record_id {identity}")
        identities.add(identity)
        address_identity = (country, _address_key(row["query"]))
        if not address_identity[1] or address_identity in address_keys:
            raise BenchmarkError(f"{path}:{line_no}: duplicate or unusable normalized address")
        address_keys.add(address_identity)
        coordinate_identity = (country, round(lat, 5), round(lon, 5))
        if coordinate_identity in coordinate_keys:
            raise BenchmarkError(f"{path}:{line_no}: duplicate address coordinate")
        coordinate_keys.add(coordinate_identity)
        counts[country] += 1
        if lineage_class == "unknown_lineage":
            unknown_counts[country] += 1
    wrong = [f"{cc}={counts[cc]}" for cc in COUNTRIES if counts[cc] < minimum]
    if wrong:
        raise BenchmarkError(
            f"truth corpus is truncated: require >= {minimum} unique rows per country; got "
            + ", ".join(wrong)
        )
    weak = [
        f"{cc}={unknown_counts[cc]}"
        for cc in COUNTRIES
        if unknown_counts[cc] < unknown_minimum
    ]
    if weak:
        raise BenchmarkError(
            "truth corpus has too few unknown_lineage rows: require >= "
            f"{unknown_minimum} per country; got " + ", ".join(weak)
        )
    if benchmark_profile == FOURSQUARE_OUTSIDE_230_PROFILE:
        if corpus_schema != TRUTH_SCHEMA:
            raise BenchmarkError(
                f"{FOURSQUARE_OUTSIDE_230_PROFILE} requires schema {TRUTH_SCHEMA} truth"
            )
        actual_counts = {country: counts[country] for country in COUNTRIES}
        if actual_counts != FOURSQUARE_OUTSIDE_230_COUNTS:
            raise BenchmarkError(
                f"{FOURSQUARE_OUTSIDE_230_PROFILE} requires exact country counts "
                f"{FOURSQUARE_OUTSIDE_230_COUNTS}; got {actual_counts}"
            )
        for line_no, row in enumerate(rows, 1):
            coordinate_records = row.get("coordinate_source_records")
            provenance = row.get("coordinate_provenance")
            lineage_evidence = row.get("lineage_evidence")
            if row.get("lineage_class") != "outside_chain":
                raise BenchmarkError(
                    f"{path}:{line_no}: {FOURSQUARE_OUTSIDE_230_PROFILE} requires "
                    "lineage_class=outside_chain"
                )
            if (
                row.get("coordinate_source_dataset") != ["Foursquare"]
                or row.get("root_source_dataset") != ["Foursquare"]
                or row.get("source_dataset") != ["Foursquare"]
                or row.get("coordinate_source_scope") != "root_fallback"
                or not isinstance(coordinate_records, list)
                or len(coordinate_records) != 1
                or not isinstance(coordinate_records[0], dict)
                or coordinate_records[0].get("dataset") != "Foursquare"
                or coordinate_records[0].get("property") not in {"", None}
                or not re.fullmatch(
                    r"[0-9a-f]{24}", str(coordinate_records[0].get("record_id", ""))
                )
                or coordinate_records[0].get("license") != "Apache-2.0"
                or row.get("root_source_records") != coordinate_records
                or not isinstance(provenance, dict)
                or provenance.get("source_name") != "Foursquare"
                or provenance.get("record_id") != coordinate_records[0].get("record_id")
                or provenance.get("license") != "Apache-2.0"
                or provenance.get("evidence_url")
                != (
                    "https://foursquare.com/placemakers/review-place/"
                    + str(coordinate_records[0].get("record_id", ""))
                )
                or row.get("license") != "Apache-2.0"
                or row.get("lineage_policy") != "overture-places-coordinate-source-v3"
            ):
                raise BenchmarkError(
                    f"{path}:{line_no}: {FOURSQUARE_OUTSIDE_230_PROFILE} requires "
                    "one exclusive Foursquare coordinate source with bound record id"
                )
            if (
                lineage_evidence != ["foursquare -> foursquare"]
            ):
                raise BenchmarkError(
                    f"{path}:{line_no}: {FOURSQUARE_OUTSIDE_230_PROFILE} requires "
                    "explicit Foursquare outside-chain lineage_evidence"
                )
        _foursquare_profile_verification(path, rows)
    return rows


def _wikidata_query(country_qid: str, language: str) -> str:
    if not language.isascii() or not language.isalpha():
        raise BenchmarkError(f"invalid Wikidata label language {language!r}")
    return f"""
SELECT DISTINCT ?item ?streetAddress ?postalCode ?municipalityLabel ?lat ?lon
                ?addressStatement ?addressReferenceUrl ?addressStatedIn ?addressStatedInLabel
                ?coordinateStatement ?coordinateReferenceUrl ?coordinateStatedIn ?coordinateStatedInLabel
WHERE {{
  ?item wdt:P17 wd:{country_qid};
        wdt:P131 ?municipality;
        p:P6375 ?addressStatement;
        p:P625 ?coordinateStatement.
  OPTIONAL {{ ?item wdt:P281 ?postalCode. }}
  ?addressStatement ps:P6375 ?streetAddress.
  ?coordinateStatement psv:P625 [
          wikibase:geoLatitude ?lat;
          wikibase:geoLongitude ?lon
        ].
  OPTIONAL {{
    ?addressStatement prov:wasDerivedFrom ?addressReference.
    OPTIONAL {{ ?addressReference pr:P854 ?addressReferenceUrl. }}
    OPTIONAL {{ ?addressReference pr:P248 ?addressStatedIn. }}
  }}
  OPTIONAL {{
    ?coordinateStatement prov:wasDerivedFrom ?coordinateReference.
    OPTIONAL {{ ?coordinateReference pr:P854 ?coordinateReferenceUrl. }}
    OPTIONAL {{ ?coordinateReference pr:P248 ?coordinateStatedIn. }}
  }}
  SERVICE wikibase:label {{
    bd:serviceParam wikibase:language "{language},en".
    ?municipality rdfs:label ?municipalityLabel.
    ?addressStatedIn rdfs:label ?addressStatedInLabel.
    ?coordinateStatedIn rdfs:label ?coordinateStatedInLabel.
  }}
}}
LIMIT 10000
""".strip()


def _fetch_country(
    country: str,
    user_agent: str,
    timeout: float,
    retrieved_at: str | None = None,
) -> list[dict]:
    user_agent = _validate_user_agent(user_agent)
    cfg = COUNTRIES[country]
    params = urllib.parse.urlencode({
        "query": _wikidata_query(cfg["qid"], cfg["language"]),
        "format": "json",
    })
    request = urllib.request.Request(
        WDQS + "?" + params,
        headers={
            "Accept": "application/sparql-results+json",
            "User-Agent": user_agent,
        },
    )
    last_error: Exception | None = None
    for attempt in range(3):
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                payload = json.load(response)
            bindings = payload["results"]["bindings"]
            break
        except (OSError, KeyError, ValueError, urllib.error.HTTPError) as exc:
            last_error = exc
            if attempt == 2:
                raise BenchmarkError(f"Wikidata fetch failed for {country}: {exc}") from exc
            retry_after = 0
            if isinstance(exc, urllib.error.HTTPError) and exc.code == 429:
                try:
                    retry_after = int(exc.headers.get("Retry-After", "60"))
                except (TypeError, ValueError):
                    retry_after = 60
            time.sleep(max(5 * (attempt + 1), retry_after))
    else:  # pragma: no cover - defensive, loop always breaks or raises
        raise BenchmarkError(f"Wikidata fetch failed for {country}: {last_error}")

    retrieved_at = retrieved_at or _utc_now()
    _validate_utc_timestamp(retrieved_at, "Wikidata retrieval timestamp")
    by_item: dict[str, dict] = {}
    rejected_items: set[str] = set()
    for binding in bindings:
        try:
            record_id = binding["item"]["value"].rsplit("/", 1)[-1]
        except (KeyError, TypeError):
            continue
        if not record_id.startswith("Q") or not record_id[1:].isdigit():
            continue
        try:
            street_address = _binding_text(binding, "streetAddress")
            postcode = _binding_text(binding, "postalCode")
            municipality = _binding_text(binding, "municipalityLabel")
            lat = float(binding["lat"]["value"])
            lon = float(binding["lon"]["value"])
            address_provenance = _statement_provenance(binding, "address")
            coordinate_statement = _statement_provenance(binding, "coordinate")
        except (BenchmarkError, KeyError, OverflowError, TypeError, ValueError):
            rejected_items.add(record_id)
            continue
        if (
            not street_address
            or not municipality
        ):
            rejected_items.add(record_id)
            continue
        if not (-90.0 <= lat <= 90.0 and -180.0 <= lon <= 180.0):
            continue
        if not _inside_product_extent(country, lat, lon):
            continue
        if not any(character.isalpha() for character in street_address) or not any(
            character.isdigit() for character in street_address
        ):
            continue
        lineage_class, coordinate_provenance = _wikidata_coordinate_provenance(
            country, record_id, retrieved_at, coordinate_statement
        )
        candidate = {
            "schema": TRUTH_SCHEMA,
            "country": country,
            "street_address": street_address,
            "postcode": postcode,
            "municipality": municipality,
            "lat": lat,
            "lon": lon,
            "record_id": record_id,
            "source_url": f"https://www.wikidata.org/wiki/{record_id}",
            "source_release": WIKIDATA_SOURCE_RELEASE,
            "source_theme": WIKIDATA_THEME,
            "address_provenance": address_provenance,
            "coordinate_provenance": coordinate_provenance,
            "license": WIKIDATA_LICENSE,
            "retrieved_at": retrieved_at,
            "lineage_class": lineage_class,
        }
        candidate["query"] = _compose_query(candidate)
        previous = by_item.get(record_id)
        candidate_order = (
            candidate["query"],
            lat,
            lon,
            json.dumps(address_provenance, sort_keys=True),
            json.dumps(coordinate_provenance, sort_keys=True),
        )
        previous_order = None if previous is None else (
            previous["query"],
            previous["lat"],
            previous["lon"],
            json.dumps(previous["address_provenance"], sort_keys=True),
            json.dumps(previous["coordinate_provenance"], sort_keys=True),
        )
        if previous_order is None or candidate_order < previous_order:
            by_item[record_id] = candidate
    rows: list[dict] = []
    seen_addresses: set[str] = set()
    seen_coordinates: set[tuple[float, float]] = set()
    for key in sorted(by_item, key=lambda value: int(value[1:])):
        if key in rejected_items:
            continue
        row = by_item[key]
        address_key = _address_key(row["query"])
        coordinate_key = (round(row["lat"], 5), round(row["lon"], 5))
        if address_key in seen_addresses or coordinate_key in seen_coordinates:
            continue
        seen_addresses.add(address_key)
        seen_coordinates.add(coordinate_key)
        rows.append(row)
    return rows


def _select_country_rows(rows: list[dict], total: int, unknown_minimum: int) -> list[dict]:
    unknown = [row for row in rows if row["lineage_class"] == "unknown_lineage"]
    if len(unknown) < unknown_minimum:
        return []
    selected = unknown[:unknown_minimum]
    selected_ids = {row["record_id"] for row in selected}
    for row in rows:
        if len(selected) >= total:
            break
        if row["record_id"] not in selected_ids:
            selected.append(row)
            selected_ids.add(row["record_id"])
    return selected


def command_fetch(args: argparse.Namespace) -> None:
    user_agent = _user_agent(args.contact)
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        raise BenchmarkError("--timeout must be finite and positive")
    if not math.isfinite(args.pause) or args.pause < 0:
        raise BenchmarkError("--pause must be finite and non-negative")
    if args.minimum < DEFAULT_MINIMUM:
        raise BenchmarkError(f"minimum must be at least {DEFAULT_MINIMUM}")
    if args.unknown_minimum < DEFAULT_UNKNOWN_MINIMUM:
        raise BenchmarkError(
            f"unknown lineage minimum must be at least {DEFAULT_UNKNOWN_MINIMUM}"
        )
    if args.unknown_minimum > args.minimum:
        raise BenchmarkError("unknown lineage minimum cannot exceed the total minimum")
    retrieved_at = _utc_now()
    all_rows: list[dict] = []
    for index, country in enumerate(COUNTRIES):
        rows = _fetch_country(country, user_agent, args.timeout, retrieved_at)
        selected = _select_country_rows(rows, args.per_country, args.unknown_minimum)
        if len(selected) < args.per_country:
            unknown_count = sum(
                row["lineage_class"] == "unknown_lineage" for row in rows
            )
            raise BenchmarkError(
                f"Wikidata returned only {len(rows)} unique address records for {country}; "
                f"{unknown_count} have unknown lineage; need {args.per_country} selected total "
                f"and {args.unknown_minimum} unknown_lineage"
            )
        all_rows.extend(selected)
        if index + 1 < len(COUNTRIES):
            time.sleep(args.pause)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    candidate = args.output.with_name(args.output.name + f".candidate-{os.getpid()}")
    candidate_manifest = _truth_manifest_path(candidate)
    recipe_argv = list(getattr(
        args,
        "_invocation_argv",
        (pathlib.Path(__file__).name, "fetch"),
    ))
    recipe_command = shlex.join(_sanitize_invocation_argv(recipe_argv))
    try:
        _atomic_jsonl(candidate, all_rows)
        rows = validate_truth(candidate, args.minimum, args.unknown_minimum)
        by_country, by_lineage, by_country_and_lineage = _lineage_counts(rows)
        manifest = {
            "schema": TRUTH_SCHEMA,
            "corpus": str(args.output),
            "sha256": _sha256(candidate),
            "rows": len(rows),
            "rows_by_country": by_country,
            "rows_by_lineage": by_lineage,
            "rows_by_country_and_lineage": by_country_and_lineage,
            "retrieved_at": retrieved_at,
            "source": WDQS,
            "licenses": [WIKIDATA_LICENSE],
            "lineage_policy": LINEAGE_POLICY,
            "source_details": {
                "dataset": WIKIDATA_DATASET,
                "theme": WIKIDATA_THEME,
                "type": WIKIDATA_TYPE,
                "source_release": WIKIDATA_SOURCE_RELEASE,
                "uri": WDQS,
                "retrieved_at": retrieved_at,
                "license": WIKIDATA_LICENSE,
            },
            "recipe": {
                "script": pathlib.Path(__file__).name,
                "script_sha256": _sha256(pathlib.Path(__file__)),
                "command": recipe_command,
            },
            "selection": (
                "first unknown-lineage records followed by numeric Wikidata QID after query, "
                "normalized-address, rounded-coordinate and product-extent filtering"
            ),
        }
        _atomic_json(candidate_manifest, manifest)
        os.replace(candidate, args.output)
        os.replace(candidate_manifest, _truth_manifest_path(args.output))
    finally:
        for leftover in (candidate, candidate_manifest):
            if leftover.exists():
                leftover.unlink()
    _validate_truth_manifest(args.output, rows)
    print(json.dumps(manifest, indent=2, sort_keys=True))


def haversine_m(lat_a: float, lon_a: float, lat_b: float, lon_b: float) -> float:
    radius = 6_371_000.0
    phi_a, phi_b = math.radians(lat_a), math.radians(lat_b)
    d_phi = math.radians(lat_b - lat_a)
    d_lambda = math.radians(lon_b - lon_a)
    h = math.sin(d_phi / 2) ** 2 + math.cos(phi_a) * math.cos(phi_b) * math.sin(d_lambda / 2) ** 2
    return 2 * radius * math.asin(math.sqrt(h))


def _validated_coordinate(engine: str, lat_value: object, lon_value: object) -> tuple[float, float]:
    try:
        lat, lon = float(lat_value), float(lon_value)
    except (OverflowError, TypeError, ValueError) as exc:
        raise BenchmarkError(f"{engine} result has invalid coordinates") from exc
    if not math.isfinite(lat) or not math.isfinite(lon):
        raise BenchmarkError(f"{engine} result has non-finite coordinates")
    if not (-90.0 <= lat <= 90.0 and -180.0 <= lon <= 180.0):
        raise BenchmarkError(f"{engine} result has out-of-range coordinates")
    return lat, lon


def _gridpin_top1(payload: object) -> tuple[float, float] | None:
    if not isinstance(payload, dict) or "results" not in payload:
        raise BenchmarkError("GridPin response must be an object with a results list")
    results = payload["results"]
    if not isinstance(results, list):
        raise BenchmarkError("GridPin response results must be a list")
    if not results:
        return None
    first = results[0]
    if not isinstance(first, dict):
        raise BenchmarkError("GridPin result is not an object")
    try:
        return _validated_coordinate("GridPin", first["lat"], first["lon"])
    except KeyError as exc:
        raise BenchmarkError("GridPin result has invalid coordinates") from exc


def _photon_top1(payload: object) -> tuple[float, float] | None:
    if not isinstance(payload, dict) or "features" not in payload:
        raise BenchmarkError("Photon response must be an object with a features list")
    features = payload["features"]
    if not isinstance(features, list):
        raise BenchmarkError("Photon response features must be a list")
    if not features:
        return None
    try:
        coordinates = features[0]["geometry"]["coordinates"]
        if not isinstance(coordinates, list) or len(coordinates) < 2:
            raise TypeError("coordinates must be a list with longitude and latitude")
        return _validated_coordinate("Photon", coordinates[1], coordinates[0])
    except (KeyError, IndexError, TypeError) as exc:
        raise BenchmarkError("Photon result has invalid GeoJSON coordinates") from exc


def _nominatim_top1(payload: object) -> tuple[float, float] | None:
    if not isinstance(payload, list):
        raise BenchmarkError("Nominatim result is not a list")
    if not payload:
        return None
    if not isinstance(payload[0], dict):
        raise BenchmarkError("Nominatim result is not an object")
    try:
        return _validated_coordinate("Nominatim", payload[0]["lat"], payload[0]["lon"])
    except KeyError as exc:
        raise BenchmarkError("Nominatim result has invalid coordinates") from exc


def _score_pairs(pairs: list[tuple[dict, tuple[float, float] | None]], threshold_m: float) -> dict:
    hits = 0
    empty = 0
    for row, answer in pairs:
        if answer is None:
            empty += 1
            continue
        if haversine_m(float(row["lat"]), float(row["lon"]), answer[0], answer[1]) <= threshold_m:
            hits += 1
    count = len(pairs)
    return {
        "cases": count,
        "hits": hits,
        "misses": count - hits,
        "empty": empty,
        "hit_at_1_pct": round(100.0 * hits / count, 3) if count else None,
    }


def _score(truth: list[dict], answers: list[tuple[float, float] | None], threshold_m: float) -> dict:
    if len(truth) != len(answers):
        raise BenchmarkError(f"answer count mismatch: truth={len(truth)}, answers={len(answers)}")
    pairs = list(zip(truth, answers))
    for index, (row, _answer) in enumerate(pairs, 1):
        if row.get("lineage_class") not in LINEAGE_CLASSES:
            raise BenchmarkError(f"truth row {index} has no valid lineage_class")
    by_country = {
        country: _score_pairs(
            [(row, answer) for row, answer in pairs if row["country"] == country],
            threshold_m,
        )
        for country in COUNTRIES
    }
    by_lineage = {
        lineage: _score_pairs(
            [(row, answer) for row, answer in pairs if row["lineage_class"] == lineage],
            threshold_m,
        )
        for lineage in LINEAGE_CLASSES
    }
    by_country_and_lineage = {
        country: {
            lineage: _score_pairs(
                [
                    (row, answer)
                    for row, answer in pairs
                    if row["country"] == country and row["lineage_class"] == lineage
                ],
                threshold_m,
            )
            for lineage in LINEAGE_CLASSES
        }
        for country in COUNTRIES
    }
    score = {
        "overall": _score_pairs(pairs, threshold_m),
        "countries": by_country,
        "lineages": by_lineage,
        "country_lineages": by_country_and_lineage,
    }
    if _corpus_schema(truth) != MULTI_SOURCE_TRUTH_SCHEMA:
        return score
    source_ids = tuple(sorted(V4_SOURCE_CATALOG))
    for index, row in enumerate(truth, 1):
        source_id = row.get("truth_source_id")
        if source_id not in V4_SOURCE_CATALOG:
            raise BenchmarkError(f"truth row {index} has no valid truth_source_id")
        if row.get("truth_source_family") != V4_SOURCE_CATALOG[source_id]["family"]:
            raise BenchmarkError(f"truth row {index} has no valid truth_source_family")
    score["sources"] = {
        source_id: _score_pairs(
            [(row, answer) for row, answer in pairs if row["truth_source_id"] == source_id],
            threshold_m,
        )
        for source_id in source_ids
    }
    score["source_lineages"] = {
        source_id: {
            lineage: _score_pairs(
                [
                    (row, answer)
                    for row, answer in pairs
                    if row["truth_source_id"] == source_id
                    and row["lineage_class"] == lineage
                ],
                threshold_m,
            )
            for lineage in LINEAGE_CLASSES
        }
        for source_id in source_ids
    }
    score["country_sources"] = {
        country: {
            source_id: _score_pairs(
                [
                    (row, answer)
                    for row, answer in pairs
                    if row["country"] == country
                    and row["truth_source_id"] == source_id
                ],
                threshold_m,
            )
            for source_id in source_ids
        }
        for country in COUNTRIES
    }
    score["country_source_lineages"] = {
        country: {
            source_id: {
                lineage: _score_pairs(
                    [
                        (row, answer)
                        for row, answer in pairs
                        if row["country"] == country
                        and row["truth_source_id"] == source_id
                        and row["lineage_class"] == lineage
                    ],
                    threshold_m,
                )
                for lineage in LINEAGE_CLASSES
            }
            for source_id in source_ids
        }
        for country in COUNTRIES
    }
    score["overall"]["interpretation"] = "descriptive_only"
    score["overall"]["headline_eligible"] = False
    return score


def _wilson_interval_95(hits: int, cases: int) -> dict:
    if cases < 0 or hits < 0 or hits > cases:
        raise BenchmarkError(f"invalid binomial count for Wilson interval: {hits}/{cases}")
    if cases == 0:
        return {
            "method": "Wilson score",
            "confidence_level": 0.95,
            "lower_pct": None,
            "upper_pct": None,
        }
    z = 1.959963984540054
    proportion = hits / cases
    denominator = 1.0 + z * z / cases
    center = (proportion + z * z / (2.0 * cases)) / denominator
    half_width = (
        z
        / denominator
        * math.sqrt(
            proportion * (1.0 - proportion) / cases
            + z * z / (4.0 * cases * cases)
        )
    )
    return {
        "method": "Wilson score",
        "confidence_level": 0.95,
        "lower_pct": round(100.0 * max(0.0, center - half_width), 3),
        "upper_pct": round(100.0 * min(1.0, center + half_width), 3),
    }


def _score_confidence_intervals(score: dict) -> dict:
    return {
        "overall": _wilson_interval_95(
            score["overall"]["hits"], score["overall"]["cases"]
        ),
        "countries": {
            country: _wilson_interval_95(
                score["countries"][country]["hits"],
                score["countries"][country]["cases"],
            )
            for country in COUNTRIES
        },
    }


def _is_hit(
    row: dict,
    answer: tuple[float, float] | None,
    threshold_m: float,
) -> bool:
    return answer is not None and haversine_m(
        float(row["lat"]),
        float(row["lon"]),
        answer[0],
        answer[1],
    ) <= threshold_m


def _exact_mcnemar_two_sided(gridpin_only: int, competitor_only: int) -> dict:
    discordant = gridpin_only + competitor_only
    if discordant == 0:
        numerator, denominator = 1, 1
    else:
        smaller = min(gridpin_only, competitor_only)
        tail_numerator = sum(
            math.comb(discordant, k) for k in range(smaller + 1)
        )
        denominator = 1 << discordant
        numerator = min(denominator, 2 * tail_numerator)
    numeric = numerator / denominator
    return {
        "method": "exact two-sided binomial on discordant pairs",
        "p_value": numeric if numeric > 0.0 else None,
        "exact_fraction": f"{numerator}/{denominator}",
    }


def _paired_slice(
    truth: list[dict],
    gridpin_answers: list[tuple[float, float] | None],
    competitor_answers: list[tuple[float, float] | None],
    threshold_m: float,
    *,
    country: str | None = None,
    inferential: bool = False,
) -> dict:
    if len(truth) != len(gridpin_answers) or len(truth) != len(competitor_answers):
        raise BenchmarkError("paired comparison answer count mismatch")
    both_hit = gridpin_only = competitor_only = both_miss = 0
    for row, gridpin_answer, competitor_answer in zip(
        truth, gridpin_answers, competitor_answers
    ):
        if country is not None and row["country"] != country:
            continue
        gridpin_hit = _is_hit(row, gridpin_answer, threshold_m)
        competitor_hit = _is_hit(row, competitor_answer, threshold_m)
        if gridpin_hit and competitor_hit:
            both_hit += 1
        elif gridpin_hit:
            gridpin_only += 1
        elif competitor_hit:
            competitor_only += 1
        else:
            both_miss += 1
    cases = both_hit + gridpin_only + competitor_only + both_miss
    discordant = gridpin_only + competitor_only
    return {
        "cases": cases,
        "both_hit": both_hit,
        "gridpin_hit_competitor_miss": gridpin_only,
        "gridpin_miss_competitor_hit": competitor_only,
        "both_miss": both_miss,
        "discordant_pairs": discordant,
        "gridpin_minus_competitor_percentage_points": (
            round(100.0 * (gridpin_only - competitor_only) / cases, 3)
            if cases
            else None
        ),
        "mcnemar": _exact_mcnemar_two_sided(gridpin_only, competitor_only),
        "interpretation": "inferential_combined" if inferential else "descriptive_only",
        "small_sample": cases < 30,
    }


def _paired_comparison(
    truth: list[dict],
    gridpin_answers: list[tuple[float, float] | None],
    competitor_answers: list[tuple[float, float] | None],
    threshold_m: float,
    *,
    inferential_combined: bool = True,
) -> dict:
    return {
        "overall": _paired_slice(
            truth,
            gridpin_answers,
            competitor_answers,
            threshold_m,
            inferential=inferential_combined,
        ),
        "countries": {
            country: _paired_slice(
                truth,
                gridpin_answers,
                competitor_answers,
                threshold_m,
                country=country,
            )
            for country in COUNTRIES
        },
        "country_slices_are_descriptive_only": True,
        "combined_sample_is_the_inferential_target": inferential_combined,
    }


def _source_relationships(engines: object, source_catalog: object) -> dict:
    if not isinstance(source_catalog, dict) or set(source_catalog) != set(V4_SOURCE_CATALOG):
        raise BenchmarkError("source relationships require the fixed schema-4 source catalog")
    if not isinstance(engines, (list, tuple, set)):
        raise BenchmarkError("source relationship engines must be a collection")
    relationships: dict[str, dict[str, dict]] = {}
    for engine in sorted(engines):
        if engine not in {"gridpin", "photon", "nominatim"}:
            raise BenchmarkError(f"unsupported source-relationship engine {engine!r}")
        relationships[engine] = {}
        for source_id in sorted(source_catalog):
            source = source_catalog[source_id]
            _require_pinned_subset(
                source,
                V4_SOURCE_CATALOG[source_id],
                f"source_catalog.{source_id}",
            )
            family = source.get("family") if isinstance(source, dict) else None
            same_osm_family = (
                engine in {"photon", "nominatim"} and family == OSM_SOURCE_FAMILY
            )
            relationship = "same_dataset_family" if same_osm_family else "unknown"
            if engine == "gridpin" and family == OSM_SOURCE_FAMILY:
                basis = (
                    "GridPin's disclosed sheet metadata does not prove or disprove a shared "
                    "upstream source with this OpenStreetMap truth input"
                )
            elif same_osm_family:
                basis = (
                    f"{engine.title()} and this truth source are both OpenStreetMap-derived"
                )
            else:
                basis = "the upstream relationship has not been established"
            relationships[engine][source_id] = {
                "truth_source_family": family,
                "relationship": relationship,
                "headline_eligible": False,
                "basis": basis,
            }
    return relationships


def _parse_sheet(values: list[str]) -> dict[str, pathlib.Path]:
    result: dict[str, pathlib.Path] = {}
    for value in values:
        if "=" not in value:
            raise BenchmarkError(f"invalid --sheet {value!r}; expected CC=/path/to/file.bin")
        country, raw_path = value.split("=", 1)
        country = country.upper()
        if country not in COUNTRIES or country in result:
            raise BenchmarkError(f"invalid or duplicate --sheet country {country!r}")
        path = pathlib.Path(raw_path).expanduser().resolve()
        if not path.is_file():
            raise BenchmarkError(f"sheet does not exist: {path}")
        result[country] = path
    missing = sorted(set(COUNTRIES) - result.keys())
    if missing:
        raise BenchmarkError(f"missing sheets for: {', '.join(missing)}")
    return result


def _source_fragments(value: object) -> list[str]:
    if isinstance(value, dict):
        return [
            fragment
            for item in value.values()
            for fragment in _source_fragments(item)
        ]
    if isinstance(value, (list, tuple)):
        return [fragment for item in value for fragment in _source_fragments(item)]
    if not isinstance(value, str) or not value.strip():
        return []
    stripped = value.strip()
    if stripped[:1] in {"{", "[", '"'}:
        try:
            decoded = json.loads(stripped)
        except json.JSONDecodeError:
            pass
        else:
            if decoded != value:
                return _source_fragments(decoded)
    return [stripped]


def _sheet_source_separation(
    country: str,
    meta: dict,
    truth_source_details: dict,
) -> dict:
    if (
        truth_source_details.get("theme") != OVERTURE_THEME
        or truth_source_details.get("type") != OVERTURE_TYPE
    ):
        raise BenchmarkError(
            "source-separation check requires truth theme=places and type=place"
        )
    if meta.get("layer") != "addresses":
        raise BenchmarkError(
            f"source-separation check requires an addresses sheet for {country}"
        )
    fragments = _source_fragments(meta.get("sources"))
    mentions_overture = any(
        _contains_normalized_token_sequence(fragment, "overture")
        for fragment in fragments
    )
    explicitly_addresses = any(
        _contains_normalized_token_sequence(fragment, "addresses")
        for fragment in fragments
    )
    mentions_truth_theme = any(
        _contains_normalized_token_sequence(fragment, OVERTURE_THEME)
        for fragment in fragments
    )
    mentions_truth_type = any(
        _contains_normalized_token_sequence(fragment, OVERTURE_TYPE)
        for fragment in fragments
    )
    if mentions_overture and (mentions_truth_theme or mentions_truth_type):
        raise BenchmarkError(
            f"GridPin {country} sheet sources identify Overture Places/place, "
            "the same truth theme/type"
        )
    if mentions_overture and not explicitly_addresses:
        raise BenchmarkError(
            f"GridPin {country} sheet sources mention Overture without explicitly "
            "identifying the addresses layer"
        )
    return {
        "sheet_layer": "addresses",
        "truth_theme": OVERTURE_THEME,
        "truth_type": OVERTURE_TYPE,
        "sheet_sources_disclosed": bool(fragments),
        "sheet_sources_mention_overture": mentions_overture,
        "overture_sheet_sources_explicitly_addresses": (
            explicitly_addresses if mentions_overture else None
        ),
        "sheet_sources_match_truth_theme_or_type": False,
        "same_export_as_indexed_sheet": False,
        "decision": "separate",
    }


def _run_gridpin(
    truth: list[dict],
    binary: pathlib.Path,
    sheets: dict[str, pathlib.Path],
    work: pathlib.Path,
    *,
    corpus_hash: str | None = None,
    binary_capture: dict | None = None,
    sheet_captures: dict[str, dict] | None = None,
    truth_source_details: dict | None = None,
) -> tuple[list[tuple[float, float] | None], dict]:
    binary_capture = binary_capture or _capture_file(binary, "GridPin binary")
    sheet_captures = sheet_captures or {
        country: _capture_file(sheet, f"GridPin {country} sheet")
        for country, sheet in sheets.items()
    }
    binary = binary_capture["_path"]
    work.mkdir(parents=True, exist_ok=True)
    run_prefix = f"gridpin-{(corpus_hash or 'adhoc')[:12]}-"
    run_work = pathlib.Path(tempfile.mkdtemp(prefix=run_prefix, dir=work))
    answer_by_identity: dict[tuple[str, str], tuple[float, float] | None] = {}
    truth_source_details = truth_source_details or {
        "dataset": OVERTURE_DATASET,
        "theme": OVERTURE_THEME,
        "type": OVERTURE_TYPE,
        "source_release": OVERTURE_RELEASE,
        "uri": OVERTURE_S3,
    }
    artifacts = {
        "binary": _public_capture(binary_capture),
        "sheets": {},
        "work_run": run_work.name,
        "commands": [],
        "source_separation": {
            "schema": SOURCE_SEPARATION_SCHEMA,
            "truth_corpus_sha256": corpus_hash,
            "truth_source": {
                key: truth_source_details[key]
                for key in ("dataset", "theme", "type", "source_release", "uri")
            },
            "sheets": {},
            "same_export_as_indexed_sheet": False,
            "decision": "separate",
        },
    }
    validated_meta: dict[str, dict] = {}

    # Preflight every sheet's embedded identity and source lineage before any
    # benchmark query is executed.  A later-country provenance failure must not
    # leave earlier countries looking like a partially valid measurement.
    for country in sheets:
        sheet_capture = sheet_captures[country]
        sheet = sheet_capture["_path"]
        _verify_captures([binary_capture, sheet_capture])
        meta_argv = [str(binary), "meta", str(sheet), "--json"]
        public_meta_argv = [
            binary_capture["path"],
            "meta",
            sheet_capture["path"],
            "--json",
        ]
        artifacts["commands"].append({
            "argv": public_meta_argv,
            "display_command": shlex.join(public_meta_argv),
        })
        meta_process = subprocess.run(
            meta_argv,
            capture_output=True,
            text=True,
            timeout=60,
        )
        _verify_captures([binary_capture, sheet_capture])
        if meta_process.returncode != 0:
            raise BenchmarkError(
                f"cannot read GridPin metadata for {country}: {meta_process.stderr.strip()[:300]}"
            )
        try:
            meta = json.loads(meta_process.stdout)
        except json.JSONDecodeError as exc:
            raise BenchmarkError(f"GridPin metadata is not JSON for {country}") from exc
        if not isinstance(meta, dict):
            raise BenchmarkError(f"GridPin metadata is not an object for {country}")
        if meta.get("country") != country.lower() or meta.get("layer") != "addresses":
            raise BenchmarkError(
                f"wrong GridPin sheet identity for {country}: "
                f"country={meta.get('country')!r}, layer={meta.get('layer')!r}"
            )
        for field in ("source_release", "license"):
            if not isinstance(meta.get(field), str) or not meta[field].strip():
                raise BenchmarkError(
                    f"GridPin sheet metadata for {country} has no non-empty {field}"
                )
        separation = _sheet_source_separation(country, meta, truth_source_details)
        separation.update({
            "sheet_sha256": sheet_capture["sha256"],
            "sheet_source_release": meta["source_release"],
            "sheet_license": meta["license"],
            "sheet_sources": meta.get("sources"),
        })
        validated_meta[country] = meta
        artifacts["source_separation"]["sheets"][country] = separation

    for country in sheets:
        sheet_capture = sheet_captures[country]
        sheet = sheet_capture["_path"]
        meta = validated_meta[country]
        artifacts["sheets"][country] = {
            **_public_capture(sheet_capture),
            "country": meta["country"],
            "layer": meta["layer"],
            "source_release": meta["source_release"],
            "license": meta["license"],
            # Do not persist arbitrary engine metadata: unknown fields have
            # historically included build-host paths.  Keep only the fields
            # actually validated by this benchmark contract.
            "meta": _sanitize_json_paths({
                key: meta[key]
                for key in SHEET_META_ALLOWLIST
                if key in meta
            }),
        }
        country_rows = [row for row in truth if row["country"] == country]
        input_path = run_work / f"gridpin-{country.lower()}-input.jsonl"
        output_path = run_work / f"gridpin-{country.lower()}-output.jsonl"
        _atomic_jsonl_noreplace(input_path, [{"q": row["query"]} for row in country_rows])
        if _path_exists_nofollow(output_path):
            raise BenchmarkError(f"fresh GridPin output path is unexpectedly occupied: {output_path}")
        _verify_captures([binary_capture, sheet_capture])
        batch_argv = [
            str(binary),
            "batch",
            str(sheet),
            str(input_path),
            str(output_path),
            "-k",
            "1",
        ]
        public_batch_argv = [
            binary_capture["path"],
            "batch",
            sheet_capture["path"],
            f"{run_work.name}/{input_path.name}",
            f"{run_work.name}/{output_path.name}",
            "-k",
            "1",
        ]
        artifacts["commands"].append({
            "argv": public_batch_argv,
            "display_command": shlex.join(public_batch_argv),
        })
        completed = subprocess.run(
            batch_argv,
            capture_output=True,
            text=True,
            timeout=900,
        )
        _verify_captures([binary_capture, sheet_capture])
        if completed.returncode != 0:
            raise BenchmarkError(
                f"GridPin batch failed for {country} with exit {completed.returncode}: "
                f"{completed.stderr.strip()[:300]}"
            )
        if not _path_exists_nofollow(output_path):
            raise BenchmarkError(
                f"GridPin batch exited successfully but did not create fresh output for {country}"
            )
        payloads = _load_jsonl_regular(
            output_path,
            f"GridPin {country} output",
            single_link=True,
        )
        if len(payloads) != len(country_rows):
            raise BenchmarkError(
                f"GridPin output is truncated for {country}: {len(payloads)} of {len(country_rows)}"
            )
        for row, payload in zip(country_rows, payloads):
            answer_by_identity[(country, row["record_id"])] = _gridpin_top1(payload)
    answers = [answer_by_identity[(row["country"], row["record_id"])] for row in truth]
    return answers, artifacts


def _canonical_endpoint(engine: str, raw_url: str) -> str:
    if engine not in {"photon", "nominatim"}:
        raise BenchmarkError(f"unknown engine {engine!r}")
    parsed = urllib.parse.urlsplit(raw_url)
    if parsed.scheme.lower() not in {"http", "https"} or not parsed.hostname:
        raise BenchmarkError(f"{engine} URL must be an absolute HTTP(S) URL")
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise BenchmarkError(f"{engine} URL must not contain credentials, query or fragment")
    suffix = "/api/" if engine == "photon" else "/search"
    path = parsed.path.rstrip("/")
    if path.endswith(suffix.rstrip("/")):
        path = path + ("/" if engine == "photon" else "")
    elif path:
        path = path + suffix
    else:
        path = suffix
    netloc = parsed.netloc.lower()
    return urllib.parse.urlunsplit((parsed.scheme.lower(), netloc, path, "", ""))


def _public_demo(engine: str, endpoint: str) -> bool:
    parsed = urllib.parse.urlsplit(endpoint)
    hostname = (parsed.hostname or "").lower().rstrip(".")
    expected = "photon.komoot.io" if engine == "photon" else "nominatim.openstreetmap.org"
    if hostname != expected:
        return False
    if parsed.scheme != "https" or parsed.port not in {None, 443}:
        raise BenchmarkError(f"public {engine} must use its canonical HTTPS origin")
    return True


def _status_endpoint(engine: str, endpoint: str) -> str:
    parsed = urllib.parse.urlsplit(endpoint)
    suffix = "/api/" if engine == "photon" else "/search"
    if not parsed.path.endswith(suffix):
        raise BenchmarkError(f"cannot derive {engine} status endpoint from {endpoint!r}")
    base = parsed.path[: -len(suffix)].rstrip("/")
    status_path = f"{base}/status" if base else "/status"
    query = "format=json" if engine == "nominatim" else ""
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, status_path, query, ""))


def _status_field(response: dict, key: str, engine: str) -> str:
    value = response.get(key)
    if not isinstance(value, str) or not _clean_text(value):
        raise BenchmarkError(f"{engine} status evidence has no non-empty {key}")
    return _clean_text(value)


def _status_summary(engine: str, response: object) -> tuple[dict, str]:
    if not isinstance(response, dict):
        raise BenchmarkError(f"{engine} status evidence response must be an object")
    if engine == "photon":
        if str(response.get("status", "")).casefold() != "ok":
            raise BenchmarkError("Photon status evidence does not report status=Ok")
        summary = {
            "version": _status_field(response, "version", engine),
            "git_commit": _status_field(response, "git_commit", engine),
            "import_date": _status_field(response, "import_date", engine),
        }
        return summary, summary["version"]
    if response.get("status") not in (0, "0"):
        raise BenchmarkError("Nominatim status evidence does not report status=0")
    update_key = "data_updated_at" if "data_updated_at" in response else "data_updated"
    summary = {
        "software_version": _status_field(response, "software_version", engine),
        "database_version": _status_field(response, "database_version", engine),
        "data_updated_at": _status_field(response, update_key, engine),
    }
    return summary, summary["software_version"]


def _load_status_evidence(
    engine: str,
    evidence_path: pathlib.Path,
    query_endpoint: str,
) -> dict:
    path = evidence_path.expanduser().resolve()
    if not path.is_file():
        raise BenchmarkError(f"{engine} status evidence does not exist: {path}")
    try:
        raw = path.read_bytes()
        evidence = json.loads(raw.decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BenchmarkError(f"cannot read {engine} status evidence {path}: {exc}") from exc
    expected_endpoint = _status_endpoint(engine, query_endpoint)
    expected = {
        "schema": STATUS_EVIDENCE_SCHEMA,
        "engine": engine,
        "endpoint": expected_endpoint,
    }
    if not isinstance(evidence, dict):
        raise BenchmarkError(f"{engine} status evidence must be a JSON object")
    for key, value in expected.items():
        if evidence.get(key) != value:
            raise BenchmarkError(
                f"{engine} status evidence mismatch for {key}: "
                f"expected {value!r}, got {evidence.get(key)!r}"
            )
    fetched_at = _validate_utc_timestamp(
        evidence.get("fetched_at"), f"{engine} status evidence fetched_at"
    )
    response = evidence.get("response")
    summary, version = _status_summary(engine, response)
    digest = hashlib.sha256(raw).hexdigest()
    capture = _capture_file(path, f"{engine} status evidence")
    if capture["sha256"] != digest:
        raise BenchmarkError(f"{engine} status evidence changed while it was being read")
    return {
        "path": capture["path"],
        "sha256": digest,
        "fetched_at": fetched_at,
        "endpoint": expected_endpoint,
        "summary": summary,
        "service_identity": f"{engine}@{query_endpoint}",
        "service_version": version,
        "_capture": capture,
    }


def _request_url(engine: str, endpoint: str, row: dict) -> str:
    if engine == "photon":
        params = {
            "q": row["query"],
            "limit": "1",
            "lang": COUNTRIES[row["country"]]["language"],
            # Photon documents ISO 3166-1 alpha-2 countrycode values in upper
            # case.  Nominatim's countrycodes parameter remains lower-case.
            "countrycode": row["country"],
        }
    else:
        params = {
            "q": row["query"],
            "format": "jsonv2",
            "limit": "1",
            "addressdetails": "1",
            "countrycodes": row["country"].lower(),
        }
    return endpoint + "?" + urllib.parse.urlencode(params)


def _cache_key(
    engine: str,
    endpoint: str,
    corpus_hash: str,
    service_identity: str,
    service_version: str,
    status_evidence_sha256: str,
) -> str:
    material = (
        f"schema={RESPONSE_CACHE_SCHEMA}\0{engine}\0{endpoint}\0{service_identity}\0"
        f"{service_version}\0{status_evidence_sha256}\0{corpus_hash}\0top1"
    ).encode("utf-8")
    return hashlib.sha256(material).hexdigest()[:20]


def _validate_cache_rows(
    cached: list[dict],
    truth: list[dict],
    engine: str,
    endpoint: str,
    corpus_hash: str,
    service_identity: str,
    service_version: str,
    status_evidence_sha256: str,
    *,
    complete: bool,
) -> list[tuple[float, float] | None]:
    if len(cached) > len(truth) or (complete and len(cached) != len(truth)):
        raise BenchmarkError(
            f"cache length mismatch: cached={len(cached)}, truth={len(truth)}, complete={complete}"
        )
    answers: list[tuple[float, float] | None] = []
    for expected, row in zip(truth, cached):
        required = {
            "schema": RESPONSE_CACHE_SCHEMA,
            "engine": engine,
            "endpoint": endpoint,
            "service_identity": service_identity,
            "service_version": service_version,
            "status_evidence_sha256": status_evidence_sha256,
            "corpus_sha256": corpus_hash,
            "country": expected["country"],
            "record_id": expected["record_id"],
            "request_url": _request_url(engine, endpoint, expected),
        }
        for key, value in required.items():
            if row.get(key) != value:
                raise BenchmarkError(f"cache mismatch for {key}: expected {value!r}, got {row.get(key)!r}")
        _validate_utc_timestamp(row.get("fetched_at"), "cache row fetched_at")
        if "response" not in row:
            raise BenchmarkError("cache row has no raw response")
        answer = _photon_top1(row["response"]) if engine == "photon" else _nominatim_top1(row["response"])
        answers.append(answer)
    return answers


def _open_partial_cache(path: pathlib.Path):
    if not _path_exists_nofollow(path):
        return None, []
    _require_regular_nofollow(path, "partial response cache", single_link=True)
    fd = _open_regular_nofollow(
        path,
        os.O_RDWR | os.O_APPEND,
        "partial response cache",
        single_link=True,
    )
    handle = os.fdopen(fd, "a+", encoding="utf-8")
    try:
        rows = _load_jsonl_stream(handle, path)
        handle.seek(0, os.SEEK_END)
        return handle, rows
    except BaseException:
        handle.close()
        raise


def _create_partial_cache(path: pathlib.Path):
    flags = (
        os.O_RDWR
        | os.O_APPEND
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_NOFOLLOW", 0)
    )
    try:
        fd = os.open(path, flags, 0o600)
    except FileExistsError as exc:
        raise BenchmarkError(f"partial response cache appeared concurrently: {path}") from exc
    except OSError as exc:
        raise BenchmarkError(f"cannot create partial response cache {path}: {exc}") from exc
    return os.fdopen(fd, "a+", encoding="utf-8")


def _resume_pause_seconds(last_fetched_at: str, pause: float) -> float:
    value = _validate_utc_timestamp(last_fetched_at, "cache row fetched_at")
    parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    elapsed = max(0.0, (dt.datetime.now(dt.timezone.utc) - parsed).total_seconds())
    return max(0.0, pause - elapsed)


def _http_answers(
    engine: str,
    truth: list[dict],
    endpoint: str,
    corpus_hash: str,
    service_identity: str,
    service_version: str,
    status_evidence_sha256: str,
    user_agent: str,
    pause: float,
    timeout: float,
    cache_path: pathlib.Path,
) -> tuple[list[tuple[float, float] | None], dict]:
    user_agent = _validate_user_agent(user_agent)
    cache_path.parent.mkdir(parents=True, exist_ok=True)
    if _path_exists_nofollow(cache_path):
        cached, cache_digest = _load_jsonl_regular_snapshot(
            cache_path, "response cache", single_link=True
        )
        answers = _validate_cache_rows(
            cached,
            truth,
            engine,
            endpoint,
            corpus_hash,
            service_identity,
            service_version,
            status_evidence_sha256,
            complete=True,
        )
    else:
        with _exclusive_claim(
            cache_path,
            "response cache",
            require_target_absent=False,
        ):
            if _path_exists_nofollow(cache_path):
                cached, cache_digest = _load_jsonl_regular_snapshot(
                    cache_path, "response cache", single_link=True
                )
                answers = _validate_cache_rows(
                    cached,
                    truth,
                    engine,
                    endpoint,
                    corpus_hash,
                    service_identity,
                    service_version,
                    status_evidence_sha256,
                    complete=True,
                )
            else:
                partial = cache_path.with_suffix(cache_path.suffix + ".partial")
                partial_handle, cached = _open_partial_cache(partial)
                try:
                    answers = _validate_cache_rows(
                        cached,
                        truth,
                        engine,
                        endpoint,
                        corpus_hash,
                        service_identity,
                        service_version,
                        status_evidence_sha256,
                        complete=False,
                    )
                    if engine == "nominatim" and cached and len(cached) < len(truth):
                        remaining = _resume_pause_seconds(cached[-1]["fetched_at"], pause)
                        if remaining > 0:
                            time.sleep(remaining)
                    for index in range(len(cached), len(truth)):
                        row = truth[index]
                        request_url = _request_url(engine, endpoint, row)
                        request = urllib.request.Request(
                            request_url,
                            headers={"Accept": "application/json", "User-Agent": user_agent},
                        )
                        try:
                            with _urlopen_no_redirect(request, timeout=timeout) as response:
                                if response.status != 200:
                                    raise BenchmarkError(f"{engine} returned HTTP {response.status}")
                                final_url = response.geturl()
                                if final_url != request_url:
                                    raise BenchmarkError(
                                        f"{engine} redirected benchmark query: "
                                        f"requested {request_url!r}, received {final_url!r}"
                                    )
                                payload = json.load(response)
                        except (OSError, ValueError, urllib.error.HTTPError) as exc:
                            raise BenchmarkError(
                                f"{engine} request failed at row {index + 1}/{len(truth)}; "
                                f"partial cache was retained and no score was published: {exc}"
                            ) from exc
                        answer = (
                            _photon_top1(payload)
                            if engine == "photon"
                            else _nominatim_top1(payload)
                        )
                        cache_row = {
                            "schema": RESPONSE_CACHE_SCHEMA,
                            "engine": engine,
                            "endpoint": endpoint,
                            "service_identity": service_identity,
                            "service_version": service_version,
                            "status_evidence_sha256": status_evidence_sha256,
                            "corpus_sha256": corpus_hash,
                            "country": row["country"],
                            "record_id": row["record_id"],
                            "request_url": request_url,
                            "fetched_at": _utc_now(),
                            "answer": list(answer) if answer is not None else None,
                            "response": payload,
                            "attribution": SERVICE_ATTRIBUTION[engine],
                        }
                        if partial_handle is None:
                            partial_handle = _create_partial_cache(partial)
                        partial_handle.write(
                            json.dumps(cache_row, ensure_ascii=False, sort_keys=True) + "\n"
                        )
                        partial_handle.flush()
                        os.fsync(partial_handle.fileno())
                        answers.append(answer)
                        cached.append(cache_row)
                        if index + 1 < len(truth):
                            time.sleep(pause)
                    if partial_handle is None:
                        partial_handle = _create_partial_cache(partial)
                    partial_handle.flush()
                    os.fsync(partial_handle.fileno())
                    partial_identity = os.fstat(partial_handle.fileno())
                    if not _same_file_identity(partial, partial_identity):
                        raise BenchmarkError("partial response cache ownership changed before publication")
                    _publish_existing_noreplace(
                        partial,
                        cache_path,
                        "response cache",
                        expected_source_identity=partial_identity,
                    )
                    if _same_file_identity(partial, partial_identity):
                        os.unlink(partial)
                        _fsync_directory(partial.parent)
                    cached, cache_digest = _load_jsonl_regular_snapshot(
                        cache_path, "response cache", single_link=True
                    )
                    answers = _validate_cache_rows(
                        cached,
                        truth,
                        engine,
                        endpoint,
                        corpus_hash,
                        service_identity,
                        service_version,
                        status_evidence_sha256,
                        complete=True,
                    )
                finally:
                    if partial_handle is not None:
                        partial_handle.close()

    fetched_times = [
        dt.datetime.fromisoformat(row["fetched_at"].replace("Z", "+00:00")).astimezone(
            dt.timezone.utc
        )
        for row in cached
    ]
    current_cache_digest = _sha256_regular(
        cache_path, "response cache", single_link=True
    )
    if current_cache_digest != cache_digest:
        raise BenchmarkError("response cache changed after its sealed read")
    artifact = {
        "path": _logical_path(cache_path),
        "sha256": cache_digest,
        "rows": len(cached),
        "engine": engine,
        "endpoint": endpoint,
        "service_identity": service_identity,
        "service_version": service_version,
        "status_evidence_sha256": status_evidence_sha256,
        "corpus_sha256": corpus_hash,
        "first_fetched_at": min(fetched_times).isoformat(),
        "last_fetched_at": max(fetched_times).isoformat(),
        "attribution": SERVICE_ATTRIBUTION[engine],
    }
    return answers, artifact


def _competitor_configuration(args: argparse.Namespace) -> tuple[dict[str, dict], dict[str, dict]]:
    selected_values = getattr(args, "competitor", None) or list(SERVICE_ATTRIBUTION)
    if len(selected_values) != len(set(selected_values)):
        raise BenchmarkError("--competitor values must not be duplicated")
    selected = set(selected_values)
    unknown = selected - SERVICE_ATTRIBUTION.keys()
    if unknown:
        raise BenchmarkError(f"unknown competitors: {', '.join(sorted(unknown))}")
    configurations: dict[str, dict] = {}
    not_run: dict[str, dict] = {}
    for engine in SERVICE_ATTRIBUTION:
        if engine not in selected:
            reason = getattr(args, f"{engine}_not_run_reason", None)
            not_run[engine] = {
                "status": "not_run",
                "reason": _clean_text(reason) or "not selected by the operator",
            }
            continue
        raw_url = getattr(args, f"{engine}_url", None)
        evidence_value = getattr(args, f"{engine}_status_evidence", None)
        selected_not_run_reason = getattr(args, f"{engine}_not_run_reason", None)
        if _clean_text(selected_not_run_reason):
            raise BenchmarkError(
                f"--{engine}-not-run-reason contradicts selected competitor {engine}"
            )
        if not isinstance(raw_url, str) or not raw_url.strip():
            raise BenchmarkError(f"--{engine}-url is required for selected competitor {engine}")
        if evidence_value is None:
            raise BenchmarkError(
                f"--{engine}-status-evidence is required for selected competitor {engine}"
            )
        endpoint = _canonical_endpoint(engine, raw_url)
        is_public_demo = _public_demo(engine, endpoint)
        if engine == "photon" and is_public_demo:
            raise BenchmarkError(
                "the public Photon demo is not permitted for the 1200-row benchmark; "
                "use a self-hosted endpoint"
            )
        try:
            evidence_path = pathlib.Path(evidence_value)
        except TypeError as exc:
            raise BenchmarkError(f"invalid --{engine}-status-evidence path") from exc
        evidence = _load_status_evidence(engine, evidence_path, endpoint)
        configurations[engine] = {
            "endpoint": endpoint,
            "identity": evidence["service_identity"],
            "version": evidence["service_version"],
            "public_demo": is_public_demo,
            "status_evidence": evidence,
        }
    return configurations, not_run


_ARGV_PATH_OPTIONS = {
    "--truth",
    "--gridpin-bin",
    "--work",
    "--output",
    "--photon-status-evidence",
    "--nominatim-status-evidence",
}
_ARGV_SECRET_OPTIONS = {"--contact", "--user-agent"}


def _sanitize_path_argv_value(value: str) -> str:
    candidate = pathlib.Path(value).expanduser()
    return _logical_path(candidate) if candidate.is_absolute() else value


def _sanitize_invocation_argv(argv: list[str] | tuple[str, ...]) -> list[str]:
    sanitized: list[str] = []
    expect_path = False
    expect_sheet = False
    expect_secret = False
    for index, token in enumerate(argv):
        if expect_path:
            sanitized.append(_sanitize_path_argv_value(token))
            expect_path = False
            continue
        if expect_sheet:
            if "=" in token:
                country, value = token.split("=", 1)
                token = f"{country}={_sanitize_path_argv_value(value)}"
            sanitized.append(token)
            expect_sheet = False
            continue
        if expect_secret:
            sanitized.append("<redacted-contact>")
            expect_secret = False
            continue
        if token in _ARGV_PATH_OPTIONS:
            sanitized.append(token)
            expect_path = True
            continue
        if token == "--sheet":
            sanitized.append(token)
            expect_sheet = True
            continue
        if token in _ARGV_SECRET_OPTIONS:
            sanitized.append(token)
            expect_secret = True
            continue
        matched = next((option for option in _ARGV_PATH_OPTIONS if token.startswith(option + "=")), None)
        if matched is not None:
            sanitized.append(f"{matched}={_sanitize_path_argv_value(token[len(matched) + 1:])}")
            continue
        if token.startswith("--sheet=") and "=" in token[len("--sheet="):]:
            country, value = token[len("--sheet="):].split("=", 1)
            sanitized.append(f"--sheet={country}={_sanitize_path_argv_value(value)}")
            continue
        secret = next(
            (option for option in _ARGV_SECRET_OPTIONS if token.startswith(option + "=")),
            None,
        )
        if secret is not None:
            sanitized.append(f"{secret}=<redacted-contact>")
            continue
        if index == 0 and pathlib.Path(token).expanduser().is_absolute():
            sanitized.append(_logical_path(pathlib.Path(token)))
        else:
            sanitized.append(token)
    return sanitized


def _reproducibility(args: argparse.Namespace) -> dict:
    raw_argv = list(getattr(args, "_invocation_argv", [pathlib.Path(__file__).name, "run"]))
    argv = _sanitize_invocation_argv(raw_argv)
    return {
        "argv": argv,
        "display_command": shlex.join(argv),
        "sanitization": "filesystem paths are logical labels; contact values are redacted",
        "python": {
            "implementation": platform.python_implementation(),
            "version": platform.python_version(),
            "executable": pathlib.Path(sys.executable).name,
        },
        "platform": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
        },
    }


def _public_status_evidence(evidence: dict) -> dict:
    return {key: value for key, value in evidence.items() if not key.startswith("_")}


def _public_gridpin_artifacts(artifacts: dict) -> dict:
    if not isinstance(artifacts, dict):
        raise BenchmarkError("GridPin artifacts must be an object")
    public = dict(artifacts)
    raw_sheets = artifacts.get("sheets")
    if not isinstance(raw_sheets, dict):
        raise BenchmarkError("GridPin artifacts have no sheets object")
    public_sheets: dict[str, dict] = {}
    for country, raw_sheet in raw_sheets.items():
        if not isinstance(raw_sheet, dict):
            raise BenchmarkError(f"GridPin artifact for {country} must be an object")
        sheet = dict(raw_sheet)
        raw_meta = raw_sheet.get("meta", {})
        if not isinstance(raw_meta, dict):
            raise BenchmarkError(f"GridPin artifact metadata for {country} must be an object")
        sheet["meta"] = {
            key: raw_meta[key]
            for key in SHEET_META_ALLOWLIST
            if key in raw_meta
        }
        public_sheets[country] = sheet
    public["sheets"] = public_sheets
    return public


def _command_run_claimed(args: argparse.Namespace, output: pathlib.Path) -> dict:
    unknown_minimum = getattr(args, "unknown_minimum", DEFAULT_UNKNOWN_MINIMUM)
    benchmark_profile = getattr(args, "benchmark_profile", None)
    if not math.isfinite(args.distance_m) or args.distance_m <= 0:
        raise BenchmarkError("--distance-m must be finite and positive")
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        raise BenchmarkError("--timeout must be finite and positive")
    if not math.isfinite(args.pause) or args.pause < 0:
        raise BenchmarkError("--pause must be finite and non-negative")
    user_agent = _validate_user_agent(args.user_agent)
    competitors, not_run = _competitor_configuration(args)
    if any(config["public_demo"] for config in competitors.values()) and not args.allow_public_services:
        raise BenchmarkError(
            "public Nominatim was selected; pass --allow-public-services only for an intentional, "
            "one-time policy-compliant run, or use a self-hosted endpoint"
        )
    if competitors.get("nominatim", {}).get("public_demo"):
        minimum_public_pause = (
            1.5 if benchmark_profile == FOURSQUARE_OUTSIDE_230_PROFILE else 1.1
        )
        if args.pause < minimum_public_pause:
            raise BenchmarkError(
                f"public Nominatim requires --pause >= {minimum_public_pause:.1f} seconds"
            )
    sheets = _parse_sheet(args.sheet)
    truth_path = args.truth.expanduser().resolve()
    manifest_path = _truth_manifest_path(truth_path)
    truth_capture = _capture_file(truth_path, "truth corpus")
    manifest_capture = _capture_file(manifest_path, "truth manifest")
    runner_capture = _capture_file(pathlib.Path(__file__), "benchmark runner")
    binary_capture = _capture_file(args.gridpin_bin, "GridPin binary")
    sheet_captures = {
        country: _capture_file(sheet, f"GridPin {country} sheet")
        for country, sheet in sheets.items()
    }
    status_captures = [
        config["status_evidence"]["_capture"] for config in competitors.values()
    ]
    input_captures = [
        truth_capture,
        manifest_capture,
        runner_capture,
        binary_capture,
        *sheet_captures.values(),
        *status_captures,
    ]
    truth = validate_truth(
        truth_capture["_path"],
        args.minimum,
        unknown_minimum,
        benchmark_profile,
    )
    truth_manifest = _validate_truth_manifest(truth_capture["_path"], truth)
    _validate_benchmark_profile_manifest(truth_manifest, benchmark_profile)
    profile_diagnostics_capture = _validate_benchmark_profile_diagnostics(
        truth_capture["_path"],
        truth,
        truth_manifest,
        benchmark_profile,
    )
    if profile_diagnostics_capture is not None:
        input_captures.append(profile_diagnostics_capture)
    truth_source_details = _validate_primary_measurement_source(truth_manifest)
    _verify_captures(input_captures)
    args.work.mkdir(parents=True, exist_ok=True)
    corpus_hash = truth_capture["sha256"]
    gridpin_answers, gridpin_artifacts = _run_gridpin(
        truth,
        binary_capture["_path"],
        sheets,
        args.work,
        corpus_hash=corpus_hash,
        binary_capture=binary_capture,
        sheet_captures=sheet_captures,
        truth_source_details=truth_source_details,
    )
    source_separation = gridpin_artifacts.get("source_separation")
    separation_sheets = (
        source_separation.get("sheets")
        if isinstance(source_separation, dict)
        else None
    )
    if (
        not isinstance(source_separation, dict)
        or source_separation.get("same_export_as_indexed_sheet") is not False
        or source_separation.get("decision") != "separate"
        or not isinstance(separation_sheets, dict)
        or set(separation_sheets) != set(sheets)
        or any(
            not isinstance(evidence, dict)
            or evidence.get("sheet_layer") != "addresses"
            or evidence.get("same_export_as_indexed_sheet") is not False
            or evidence.get("decision") != "separate"
            for evidence in separation_sheets.values()
        )
    ):
        raise BenchmarkError("GridPin run produced no valid source-separation evidence")
    gridpin_artifacts = _public_gridpin_artifacts(gridpin_artifacts)
    competitor_results: dict[str, dict] = {}
    competitor_answers: dict[str, list[tuple[float, float] | None]] = {}
    response_caches: dict[str, dict] = {}
    cache_captures: list[dict] = []
    for engine, config in competitors.items():
        _verify_captures(input_captures)
        evidence_hash = config["status_evidence"]["sha256"]
        cache_id = _cache_key(
            engine,
            config["endpoint"],
            corpus_hash,
            config["identity"],
            config["version"],
            evidence_hash,
        )
        cache_path = args.work / (
            f"{engine}-{cache_id}.jsonl"
        )
        answers, artifact = _http_answers(
            engine,
            truth,
            config["endpoint"],
            corpus_hash,
            config["identity"],
            config["version"],
            evidence_hash,
            user_agent,
            args.pause,
            args.timeout,
            cache_path,
        )
        score = _score(truth, answers, args.distance_m)
        score["status_evidence_sha256"] = evidence_hash
        competitor_results[engine] = score
        competitor_answers[engine] = answers
        cache_capture = _capture_regular_nofollow(
            cache_path,
            f"{engine} response cache",
            single_link=True,
        )
        if cache_capture["sha256"] != artifact["sha256"]:
            raise BenchmarkError(f"{engine} response cache changed while it was being captured")
        cache_captures.append(cache_capture)
        response_caches[engine] = artifact
        _verify_captures(input_captures)
    results = {"gridpin": _score(truth, gridpin_answers, args.distance_m)}
    results.update(competitor_results)
    corpus_schema = _corpus_schema(truth)
    truth_result = {
        "path": truth_capture["path"],
        "sha256": corpus_hash,
        "manifest_path": manifest_capture["path"],
        "manifest_sha256": manifest_capture["sha256"],
        "licenses": truth_manifest["licenses"],
        "rows": len(truth),
        "rows_by_country": truth_manifest["rows_by_country"],
        "rows_by_lineage": truth_manifest["rows_by_lineage"],
        "rows_by_country_and_lineage": truth_manifest["rows_by_country_and_lineage"],
        "lineage_policy": truth_manifest["lineage_policy"],
        "recipe": truth_manifest["recipe"],
    }
    if corpus_schema == MULTI_SOURCE_TRUTH_SCHEMA:
        truth_result.update({
            "assembled_at": truth_manifest["assembled_at"],
            "source_catalog": truth_manifest["source_catalog"],
            "rows_by_source": truth_manifest["rows_by_source"],
            "rows_by_country_and_source": truth_manifest["rows_by_country_and_source"],
            "rows_by_country_source_and_lineage": (
                truth_manifest["rows_by_country_source_and_lineage"]
            ),
        })
    else:
        truth_result.update({
            "retrieved_at": truth_manifest["retrieved_at"],
            "source_details": truth_manifest["source_details"],
        })
    if profile_diagnostics_capture is not None:
        truth_result.update({
            "benchmark_profile": benchmark_profile,
            "selection": truth_manifest["selection"],
            "diagnostics": _public_capture(profile_diagnostics_capture),
        })
    result = {
        "schema": corpus_schema,
        "generated_at": _utc_now(),
        "truth": truth_result,
        "metric": {"name": "hit@1", "maximum_distance_m": args.distance_m},
        "validation": {
            "benchmark_profile": benchmark_profile or "standard",
            "applied_thresholds": {
                "minimum_rows_per_country": args.minimum,
                "minimum_unknown_lineage_rows_per_country": unknown_minimum,
            },
            "default_thresholds": {
                "minimum_rows_per_country": DEFAULT_MINIMUM,
                "minimum_unknown_lineage_rows_per_country": DEFAULT_UNKNOWN_MINIMUM,
            },
        },
        "runner": _public_capture(runner_capture),
        "reproducibility": _reproducibility(args),
        "endpoints": {
            engine: {
                "url": config["endpoint"],
                "identity": config["identity"],
                "version": config["version"],
                "public_demo": config["public_demo"],
                "attribution": SERVICE_ATTRIBUTION[engine],
                "status_evidence": _public_status_evidence(config["status_evidence"]),
            }
            for engine, config in competitors.items()
        },
        "not_run": not_run,
        "response_caches": response_caches,
        "source_separation": source_separation,
        "gridpin_artifacts": gridpin_artifacts,
        "results": results,
        "statistical_analysis": {
            "confidence_intervals_95": {
                engine: _score_confidence_intervals(score)
                for engine, score in results.items()
            },
            "paired_comparisons": {
                f"gridpin_vs_{engine}": _paired_comparison(
                    truth,
                    gridpin_answers,
                    answers,
                    args.distance_m,
                    inferential_combined=(
                        corpus_schema != MULTI_SOURCE_TRUTH_SCHEMA
                    ),
                )
                for engine, answers in competitor_answers.items()
            },
        },
    }
    if corpus_schema == MULTI_SOURCE_TRUTH_SCHEMA:
        result["comparison_design"] = {
            "truth_corpus": "one_frozen_corpus_shared_by_all_engines",
            "truth_corpus_sha256": corpus_hash,
            "mixed_source_overall": "descriptive_only",
            "headline_eligible": False,
        }
        result["source_relationships"] = _source_relationships(
            tuple(results), truth_manifest["source_catalog"]
        )
    result = _sanitize_json_paths(result)
    _atomic_json_noreplace(
        output,
        result,
        before_publish=lambda: _verify_captures(input_captures + cache_captures),
    )
    return result


def command_run(args: argparse.Namespace) -> None:
    output = pathlib.Path(args.output)
    with _exclusive_claim(output, "result output", require_target_absent=True):
        result = _command_run_claimed(args, output)
    print(json.dumps(result, indent=2, sort_keys=True))


def command_validate(args: argparse.Namespace) -> None:
    benchmark_profile = getattr(args, "benchmark_profile", None)
    unknown_minimum = getattr(args, "unknown_minimum", DEFAULT_UNKNOWN_MINIMUM)
    truth_path = args.truth.expanduser().resolve()
    rows = validate_truth(
        truth_path,
        args.minimum,
        unknown_minimum,
        benchmark_profile,
    )
    manifest = _validate_truth_manifest(truth_path, rows)
    _validate_benchmark_profile_manifest(manifest, benchmark_profile)
    profile_diagnostics_capture = _validate_benchmark_profile_diagnostics(
        truth_path,
        rows,
        manifest,
        benchmark_profile,
    )
    by_country, by_lineage, by_country_and_lineage = _lineage_counts(rows)
    corpus_schema = _corpus_schema(rows)
    summary = {
        "sha256": _sha256(truth_path),
        "manifest_sha256": _sha256(_truth_manifest_path(truth_path)),
        "rows": len(rows),
        "rows_by_country": by_country,
        "rows_by_lineage": by_lineage,
        "rows_by_country_and_lineage": by_country_and_lineage,
        "recipe": manifest["recipe"],
        "status": "valid",
        "validation": {
            "benchmark_profile": benchmark_profile or "standard",
            "applied_thresholds": {
                "minimum_rows_per_country": args.minimum,
                "minimum_unknown_lineage_rows_per_country": unknown_minimum,
            },
            "default_thresholds": {
                "minimum_rows_per_country": DEFAULT_MINIMUM,
                "minimum_unknown_lineage_rows_per_country": DEFAULT_UNKNOWN_MINIMUM,
            },
        },
    }
    if profile_diagnostics_capture is not None:
        summary["diagnostics"] = _public_capture(profile_diagnostics_capture)
    if corpus_schema == MULTI_SOURCE_TRUTH_SCHEMA:
        summary.update({
            "schema": corpus_schema,
            "assembled_at": manifest["assembled_at"],
            "source_catalog": manifest["source_catalog"],
            "rows_by_source": manifest["rows_by_source"],
            "rows_by_country_and_source": manifest["rows_by_country_and_source"],
            "rows_by_country_source_and_lineage": (
                manifest["rows_by_country_source_and_lineage"]
            ),
        })
    else:
        summary.update({
            "retrieved_at": manifest["retrieved_at"],
            "source_details": manifest["source_details"],
        })
    print(json.dumps(summary, indent=2, sort_keys=True))


def command_capture_status(args: argparse.Namespace) -> None:
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        raise BenchmarkError("--timeout must be finite and positive")
    user_agent = _validate_user_agent(args.user_agent)
    query_endpoint = _canonical_endpoint(args.engine, args.url)
    status_endpoint = _status_endpoint(args.engine, query_endpoint)
    output = pathlib.Path(args.output)
    with _exclusive_claim(output, "status evidence output", require_target_absent=True):
        request = urllib.request.Request(
            status_endpoint,
            headers={"Accept": "application/json", "User-Agent": user_agent},
        )
        try:
            with _urlopen_no_redirect(request, timeout=args.timeout) as response:
                if response.status != 200:
                    raise BenchmarkError(
                        f"{args.engine} status endpoint returned HTTP {response.status}"
                    )
                final_url = response.geturl()
                if final_url != status_endpoint:
                    raise BenchmarkError(
                        f"{args.engine} redirected status capture: "
                        f"requested {status_endpoint!r}, received {final_url!r}"
                    )
                raw_response = json.load(response)
        except (OSError, ValueError, urllib.error.HTTPError) as exc:
            raise BenchmarkError(f"{args.engine} status capture failed: {exc}") from exc
        _status_summary(args.engine, raw_response)
        evidence = {
            "schema": STATUS_EVIDENCE_SCHEMA,
            "engine": args.engine,
            "endpoint": status_endpoint,
            "fetched_at": _utc_now(),
            "response": raw_response,
        }
        _validate_utc_timestamp(evidence["fetched_at"], "status evidence fetched_at")
        _atomic_json_noreplace(output, evidence)
    print(json.dumps(evidence, indent=2, sort_keys=True))


def command_self_test(_args: argparse.Namespace) -> None:
    truth: list[dict] = []
    good: list[tuple[float, float]] = []
    mutant: list[tuple[float, float]] = []
    for country_index, country in enumerate(COUNTRIES):
        for row_index in range(DEFAULT_MINIMUM):
            lat = 42.0 + country_index + row_index / 100_000.0
            lon = 2.0 + row_index / 100_000.0
            truth.append({
                "country": country,
                "lat": lat,
                "lon": lon,
                "lineage_class": "unknown_lineage",
            })
            payload = {"results": [{"lat": lat, "lon": lon}]}
            parsed = _gridpin_top1(payload)
            assert parsed is not None
            good.append(parsed)
            mutant.append((parsed[1], parsed[0]))  # reverse the parser's latitude/longitude mapping
    baseline = _score(truth, good, DEFAULT_DISTANCE_M)["overall"]["hit_at_1_pct"]
    mutated = _score(truth, mutant, DEFAULT_DISTANCE_M)["overall"]["hit_at_1_pct"]
    if baseline != 100.0 or mutated >= baseline or mutated > 1.0:
        raise BenchmarkError(f"non-vacuity mutation was not detected: baseline={baseline}, mutant={mutated}")
    print(json.dumps({
        "status": "passed",
        "cases": len(truth),
        "baseline_hit_at_1_pct": baseline,
        "coordinate_order_mutant_hit_at_1_pct": mutated,
    }, indent=2, sort_keys=True))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    fetch = subparsers.add_parser(
        "fetch",
        help="download a validate-only CC0 Wikidata diagnostic corpus",
    )
    fetch.add_argument("--output", type=pathlib.Path, default=pathlib.Path("public-bench-work/truth.jsonl"))
    fetch.add_argument("--contact", required=True, help="email or URL for the identifying User-Agent")
    fetch.add_argument("--minimum", type=int, default=DEFAULT_MINIMUM)
    fetch.add_argument("--unknown-minimum", type=int, default=DEFAULT_UNKNOWN_MINIMUM)
    fetch.add_argument("--per-country", type=int, default=DEFAULT_MINIMUM)
    fetch.add_argument("--timeout", type=float, default=120.0)
    fetch.add_argument("--pause", type=float, default=10.0)
    fetch.set_defaults(func=command_fetch)

    validate = subparsers.add_parser("validate", help="validate corpus size, provenance and license")
    validate.add_argument("truth", type=pathlib.Path)
    validate.add_argument("--minimum", type=int, default=DEFAULT_MINIMUM)
    validate.add_argument("--unknown-minimum", type=int, default=DEFAULT_UNKNOWN_MINIMUM)
    validate.add_argument(
        "--benchmark-profile",
        choices=(FOURSQUARE_OUTSIDE_230_PROFILE,),
        help="fail-closed validation profile for an explicitly approved small corpus",
    )
    validate.set_defaults(func=command_validate)

    capture_status = subparsers.add_parser(
        "capture-status",
        help="capture raw, endpoint-bound Photon or Nominatim status evidence",
    )
    capture_status.add_argument("--engine", choices=tuple(SERVICE_ATTRIBUTION), required=True)
    capture_status.add_argument(
        "--url",
        required=True,
        help="service origin, path prefix, or canonical query endpoint",
    )
    capture_status.add_argument("--output", type=pathlib.Path, required=True)
    capture_status.add_argument("--user-agent", required=True)
    capture_status.add_argument("--timeout", type=float, default=30.0)
    capture_status.set_defaults(func=command_capture_status)

    run = subparsers.add_parser(
        "run",
        help="benchmark on a validated pinned schema-v3 or hybrid schema-v4 corpus",
    )
    run.add_argument("--truth", type=pathlib.Path, required=True)
    run.add_argument("--gridpin-bin", type=pathlib.Path, required=True)
    run.add_argument("--sheet", action="append", default=[], metavar="CC=PATH")
    run.add_argument(
        "--competitor",
        action="append",
        choices=tuple(SERVICE_ATTRIBUTION),
        help="competitor to run (repeatable); defaults to both",
    )
    for engine, endpoint_name in (("photon", "/api/"), ("nominatim", "/search")):
        run.add_argument(
            f"--{engine}-url",
            help=f"operator-approved {engine} origin, path prefix, or full {endpoint_name} endpoint",
        )
        run.add_argument(
            f"--{engine}-status-evidence",
            type=pathlib.Path,
            help=(
                f"retained schema-{STATUS_EVIDENCE_SCHEMA} JSON capture of the selected "
                f"{engine} status endpoint"
            ),
        )
        run.add_argument(
            f"--{engine}-not-run-reason",
            help=f"reason {engine} was not selected (recorded without a score)",
        )
    run.add_argument("--allow-public-services", action="store_true")
    run.add_argument("--user-agent", required=True)
    run.add_argument("--minimum", type=int, default=DEFAULT_MINIMUM)
    run.add_argument("--unknown-minimum", type=int, default=DEFAULT_UNKNOWN_MINIMUM)
    run.add_argument(
        "--benchmark-profile",
        choices=(FOURSQUARE_OUTSIDE_230_PROFILE,),
        help="fail-closed validation profile for an explicitly approved small corpus",
    )
    run.add_argument("--distance-m", type=float, default=DEFAULT_DISTANCE_M)
    run.add_argument("--pause", type=float, default=1.1)
    run.add_argument("--timeout", type=float, default=30.0)
    run.add_argument("--work", type=pathlib.Path, default=pathlib.Path("public-bench-work/cache"))
    run.add_argument("--output", type=pathlib.Path, default=pathlib.Path("public-bench-work/results.json"))
    run.set_defaults(func=command_run)

    self_test = subparsers.add_parser("self-test", help="prove the scorer detects a coordinate-order mutant")
    self_test.set_defaults(func=command_self_test)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    args._invocation_argv = tuple(sys.argv)
    try:
        if args.command == "fetch" and args.per_country < args.minimum:
            raise BenchmarkError("--per-country must be at least --minimum")
        args.func(args)
        return 0
    except (BenchmarkError, subprocess.TimeoutExpired) as exc:
        print(f"public benchmark: FAIL: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
