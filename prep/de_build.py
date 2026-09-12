#!/usr/bin/env python3
"""One-shot, fail-closed orchestrator for the pinned Germany address build.

The command is deliberately narrower than a general build tool.  It performs one
DE build from Overture 2026-08-19.0, leaves an irrevocable receipt in the shared
Git common directory, and has no force/resume/cleanup mode.  Importing this module
does not inspect Git, touch the filesystem, or start a subprocess.
"""
from __future__ import annotations

import argparse
import contextlib
import errno
import fcntl
import hashlib
import importlib.util
import json
import os
import pathlib
import re
import signal
import stat
import subprocess
import sys
import tempfile
import time
import types
import uuid
from dataclasses import dataclass, replace
from typing import Any, Callable, Iterator, Mapping, Sequence

from de_sources import DE_COVERAGE, DE_EXPECTED_LAND_ROWS, DE_EXPECTED_ROWS


CODE = pathlib.Path(__file__).resolve().parent.parent
DE_RELEASE = "2026-08-19.0"
EXPECTED_ROWS = DE_EXPECTED_ROWS
DUCKDB_VERSION = "1.5.3"

GIB = 2**30
FLOOR_BYTES = 5 * GIB
SPILL_BYTES = 5 * GIB
ATTEMPT3_SPILL_BYTES = 32 * GIB

# Measured largest existing artifacts, with the accepted 1.25 safety factor.
NORM_BUDGET_BYTES = round(0.704550 * 1.25 * GIB)
EXPORT_BUDGET_BYTES = round(0.319242 * 1.25 * GIB)
BIN_BUDGET_BYTES = round(0.339900 * 1.25 * GIB)
WORKTREE_BUDGET_BYTES = round(0.020626 * GIB)
PERSISTENT_BUDGET_BYTES = (
    NORM_BUDGET_BYTES + EXPORT_BUDGET_BYTES + BIN_BUDGET_BYTES + WORKTREE_BUDGET_BYTES
)
INITIAL_REQUIRED_BYTES = FLOOR_BYTES + SPILL_BYTES + PERSISTENT_BUDGET_BYTES
ATTEMPT3_INITIAL_REQUIRED_BYTES = (
    FLOOR_BYTES + ATTEMPT3_SPILL_BYTES + PERSISTENT_BUDGET_BYTES
)

_SHA40 = re.compile(r"[0-9a-f]{40}")
_RUN_ID = re.compile(r"[0-9a-f]{32}")
_JSON_LIMIT = 16 * 2**20
_STATE_STEM = f"gridpin-de-{DE_RELEASE}"

# The recovery surface is intentionally a single, fixed authorization rather
# than a reusable retry mechanism.  Changing any of these anchors requires a
# new reviewed commit and therefore a different --expected-sha.
RECOVERY_ATTEMPT = 2
FIRST_RUN_ID = "f1e5af95397a4fd59ace7ab957ffc132"
FIRST_RECEIPT_SHA256 = "b2afd6b3f75b3ced5ae4eee8077a728ea3a597b0c4d11b0fba928bd6c8628009"
FIRST_STATS_SHA256 = "200e1009eca76748c6bb29a2000d3eb68914da85f769ede8fa88f1708c0e58c4"
FIRST_BUILDER_SHA = "175e2919c9ec5d3293306bb51fea2399abebf7da"
RECOVERY_BASE_SHA = "a49040be911589cf88b0368c599dc3715cd19176"
RECOVERY_AUTH_RELATIVE = pathlib.Path(
    "chat/response_20260821_130350_otmashka_vladeltsa_de_f4_recovery.html"
)
RECOVERY_AUTH_SHA256 = "c17a532b6699709c8c3e37cd8913f4f44a386316466656d90dc41b72c2dae00d"
FIRST_EVIDENCE_SHA256 = {
    "events.jsonl": "f45a3a906020fa347fae8d1a3e52f62ea981893430d65a6bcba7b465ce9a141b",
    "extract.stderr.log": "29e1e3e80e7b1d239b28ea6d44fd7b01d1379f02b0d5e8b15e9ee8c6de00e690",
    "extract.stdout.log": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
}
ATTEMPT2_RECEIPT_NAME = f"{_STATE_STEM}.attempt-2.receipt.json"
ATTEMPT1_SNAPSHOT_NAME = f"{_STATE_STEM}.attempt-1.snapshot"

# Attempt #3 is another fixed, owner-authorized one-shot surface.  The prior
# attempt's limits and receipt contract remain immutable; only this surface may
# use the explicitly approved 32 GiB spill ceiling.
RECOVERY3_ATTEMPT = 3
SECOND_RUN_ID = "db5233fce357436ab8e288071e24e2f2"
SECOND_RECEIPT_SHA256 = "58c4b56b352b8bda922c3e68ffcd851bed47dcf9961c946d063bd207c346929c"
SECOND_STATS_SHA256 = "231c0a64e3e6ae2f2ca39184ca0263e45474c3b8819fedddded5ca9f0c862304"
SECOND_BUILDER_SHA = "6a2ae0fca7278705b9117edf57e3de54c96b20ba"
SECOND_EVIDENCE_SHA256 = {
    "events.jsonl": "14117b04b5b29a3f4d2952e29e5e9abcb53c87e3534926c358cc04e6ba5504d5",
    "extract.stderr.log": "1f33fc4a6c7645a6e89ffa9118a0f728fc717d26a1a8cd8d3d0b8c239f2ea58a",
    "extract.stdout.log": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
}
SECOND_EVIDENCE_TREE_SHA256 = (
    "6f5ac920400c52047171367db56814579900f91a536c162b3e23145951bd352d"
)
ATTEMPT1_SNAPSHOT_MANIFEST_SHA256 = (
    "b7386ec65b05f1d29c6063ec1e6196ccbc0a8faddba0e73b3246811e5e58d4de"
)
RECOVERY3_BASE_SHA = "29bde5801dea341dc938000332c812914f32e381"
RECOVERY3_AUTH_RELATIVE = pathlib.Path(
    "chat/response_20260821_143520_otmashka_vladeltsa_de_f4_attempt3.html"
)
RECOVERY3_AUTH_SHA256 = "2eb007fae48d22524aa838f58f61ff77a8a4aabef7a092e7b0e8989e9ca897ce"
ATTEMPT3_RECEIPT_NAME = f"{_STATE_STEM}.attempt-3.receipt.json"
ATTEMPT2_SNAPSHOT_NAME = f"{_STATE_STEM}.attempt-2.snapshot"
_RECOVERY_COPY_LIMIT = 128 * 2**20
_CONTROLLED_ENV = (
    "GRIDPIN_THREADS",
    "GRIDPIN_MEM",
    "GRIDPIN_DUCKDB_TEMP_DIR",
    "GRIDPIN_DUCKDB_MAX_TEMP_BYTES",
    "GRIDPIN_REQUIRE_META",
)

_REQUIRED_SCRIPTS = (
    "prep/overture.py",
    "prep/export_build.py",
    "prep/de_sources.py",
    "tools/sheet_meta_gate.py",
)
_REQUIRED_MODELS = ("ml/parser_v0.bin", "ml/rank_v0.bin")
_GRIDPIN_BINARY = "gridpin/target/release/gridpin"
_REQUIRED_RULE_TSV = (
    "abbrev2.tsv",
    "affix_extra.tsv",
    "capitals.tsv",
    "city_alias.tsv",
    "commune_alias.tsv",
    "countries_mid.tsv",
    "countries_tail.tsv",
    "fr_ord_cities.tsv",
    "noise.tsv",
    "noise_after.tsv",
    "place_junk.tsv",
    "place_prefix.tsv",
    "place_type_strip.tsv",
    "region_markers.tsv",
    "street_types_cyr.tsv",
    "street_types_extra.tsv",
    "street_types_latin.tsv",
)
_PREFLIGHT_TEXT_MAX = 16 * 2**20
_PREFLIGHT_MODEL_MAX = 128 * 2**20
_PREFLIGHT_BINARY_MAX = 256 * 2**20
_PREFLIGHT_SHEET_MAX = 64 * 2**20
_PREFLIGHT_TIMEOUT_SECONDS = 60

_MINI_CSV = (
    b"nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,"
    b"nom_voie,nom_commune\n"
    b"hauptstrasse,00001,teststadt,01067,1,,13.7373,51.0504,Hauptstrasse,Teststadt\n"
)

# This source is executed only by the current interpreter in a separate process.
# It contains no INSTALL, URL or scan operation: with both automatic extension
# settings disabled, LOAD can only use already-installed local extension bytes.
_DUCKDB_PROBE = r"""
import json
import duckdb

EXPECTED = "1.5.3"
if duckdb.__version__ != EXPECTED:
    raise RuntimeError(f"duckdb package version {duckdb.__version__!r}, expected {EXPECTED!r}")
con = duckdb.connect(
    database=":memory:",
    config={
        "autoinstall_known_extensions": "false",
        "autoload_known_extensions": "false",
    },
)
engine_version = str(con.execute("SELECT version()").fetchone()[0]).removeprefix("v")
if engine_version != EXPECTED:
    raise RuntimeError(f"duckdb engine version {engine_version!r}, expected {EXPECTED!r}")
for extension in ("httpfs", "spatial"):
    con.execute("LOAD " + extension)
settings = dict(con.execute(
    "SELECT name, value FROM duckdb_settings() "
    "WHERE name IN ('autoinstall_known_extensions','autoload_known_extensions')"
).fetchall())
extensions = {
    name: {"loaded": bool(loaded), "installed": bool(installed)}
    for name, loaded, installed in con.execute(
        "SELECT extension_name, loaded, installed FROM duckdb_extensions() "
        "WHERE extension_name IN ('httpfs','spatial') ORDER BY extension_name"
    ).fetchall()
}
if settings != {
    "autoinstall_known_extensions": "false",
    "autoload_known_extensions": "false",
}:
    raise RuntimeError(f"unsafe DuckDB automatic extension settings: {settings!r}")
if set(extensions) != {"httpfs", "spatial"} or not all(
    item["loaded"] and item["installed"] for item in extensions.values()
):
    raise RuntimeError(f"required extensions are not locally loaded: {extensions!r}")
print(json.dumps({
    "package_version": duckdb.__version__,
    "engine_version": engine_version,
    "settings": settings,
    "extensions": extensions,
}, sort_keys=True, separators=(",", ":")))
""".strip()

# The stage is intentionally detached so a group can be stopped atomically,
# but a detached child would otherwise survive SIGKILL of this orchestrator.
# This tiny isolated wrapper owns the real stage and watches an inherited pipe:
# parent death closes the writer, EOF triggers TERM/KILL of the entire stage
# group, and the wrapper does not exit until that group is gone.
_STAGE_GUARD = r"""
import json
import os
import select
import signal
import subprocess
import sys
import time

guard_fd = int(sys.argv[1])
argv = json.loads(sys.argv[2])
if not isinstance(argv, list) or not argv or not all(isinstance(x, str) for x in argv):
    raise SystemExit(126)

requested_signal = 0
child_exited = False

def request_stop(signum, _frame):
    global requested_signal
    requested_signal = signum

def note_child_exit(_signum, _frame):
    global child_exited
    child_exited = True

def group_signalable(child):
    try:
        os.killpg(child.pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # On macOS a group containing only our unreaped zombie leader answers
        # EPERM.  The zombie pins the pgid, so it cannot have been reused.
        if child_exited:
            return False
        raise
    return True

def linux_group_has_live_members(pgid, proc_root="/proc"):
    # Ignore dead group members without reaping the leader that pins pgid.
    uncertain = False
    try:
        with os.scandir(proc_root) as scan:
            entries = list(scan)
    except OSError:
        # A missing/unreadable witness must fail closed under the parent flock.
        return True
    for entry in entries:
        if not entry.name.isdecimal():
            continue
        try:
            with open(os.path.join(proc_root, entry.name, "stat"), "rb") as stream:
                raw = stream.read()
        except (FileNotFoundError, ProcessLookupError):
            continue
        except OSError:
            uncertain = True
            continue
        marker = raw.rfind(b") ")
        fields = raw[marker + 2:].split() if marker >= 0 else []
        if len(fields) < 3:
            uncertain = True
            continue
        try:
            member_pgid = int(fields[2])
        except ValueError:
            uncertain = True
            continue
        if member_pgid == pgid and fields[0] not in (b"Z", b"X", b"x"):
            return True
    return uncertain

def group_has_live_members(child):
    if sys.platform.startswith("linux"):
        return linux_group_has_live_members(child.pid)
    return group_signalable(child)

def signal_group(child, signum):
    try:
        os.killpg(child.pid, signum)
    except ProcessLookupError:
        return False
    except PermissionError:
        if child_exited:
            return False
        raise
    return True

def stop_group(child):
    signal_group(child, signal.SIGTERM)
    deadline = time.monotonic() + 10
    while not child_exited and group_signalable(child) and time.monotonic() < deadline:
        time.sleep(0.01)
    # Two empty observations around a scheduler turn close the fork-vs-kill
    # race.  Linux observes non-zombie /proc members because killpg(pgid, 0)
    # remains successful for a zombie-only group until its leader is reaped.
    empty_rounds = 0
    while empty_rounds < 2:
        if group_has_live_members(child):
            signal_group(child, signal.SIGKILL)
            empty_rounds = 0
        else:
            empty_rounds += 1
        if empty_rounds < 2:
            time.sleep(0.01)
    # Reap only after the anchored group contains no live members.  The zombie
    # leader pins pgid across every preceding observation/signal; after wait(),
    # this wrapper performs no further pgid operation.
    return child.wait()

signal.signal(signal.SIGTERM, request_stop)
signal.signal(signal.SIGINT, request_stop)
signal.signal(signal.SIGCHLD, note_child_exit)

# Refuse to start a stage if the parent died before the wrapper initialized.
ready, _, _ = select.select([guard_fd], [], [], 0)
if ready and os.read(guard_fd, 1) == b"":
    raise SystemExit(125)

child = subprocess.Popen(argv, start_new_session=True)
while True:
    if requested_signal:
        stop_group(child)
        raise SystemExit(128 + requested_signal)
    ready, _, _ = select.select([guard_fd], [], [], 0.1)
    if ready and os.read(guard_fd, 1) == b"":
        stop_group(child)
        raise SystemExit(125)
    if child_exited:
        rc = stop_group(child)
        raise SystemExit(rc)
""".strip()


class BuildRefused(RuntimeError):
    """The one-shot build could not safely begin or continue."""


class BuildAlreadyRunning(BuildRefused):
    """Another process owns the shared nonblocking flock."""


class ReceiptExists(BuildRefused):
    """The one-shot receipt already exists, including after a crashed run."""


class LowDisk(BuildRefused):
    """A preflight or continuous free-space floor was crossed."""


class StageFailed(BuildRefused):
    """A child stage returned a non-zero status."""


class SystemServices:
    """Injectable operating-system boundary used by the offline unit tests."""

    def run(self, argv: Sequence[str], **kwargs: Any) -> subprocess.CompletedProcess[Any]:
        return subprocess.run(argv, **kwargs)

    def popen(self, argv: Sequence[str], **kwargs: Any) -> subprocess.Popen[Any]:
        return subprocess.Popen(argv, **kwargs)

    def popen_guarded(
        self,
        argv: Sequence[str],
        *,
        guard_read_fd: int,
        **kwargs: Any,
    ) -> subprocess.Popen[Any]:
        wrapper_argv = [
            sys.executable,
            "-I",
            "-c",
            _STAGE_GUARD,
            str(guard_read_fd),
            json.dumps(list(argv), ensure_ascii=True, separators=(",", ":")),
        ]
        process = subprocess.Popen(
            wrapper_argv,
            pass_fds=(guard_read_fd,),
            **kwargs,
        )
        # The parent must never SIGKILL this cooperative wrapper before it has
        # finished stopping the separately detached real-stage group.
        process._gridpin_parent_death_guard = True  # type: ignore[attr-defined]
        return process

    def statvfs(self, path: str) -> os.statvfs_result:
        return os.statvfs(path)

    def sleep(self, seconds: float) -> None:
        time.sleep(seconds)

    def load_de_sources(self, code_root: pathlib.Path) -> types.ModuleType:
        path = code_root / "prep" / "de_sources.py"
        spec = importlib.util.spec_from_file_location("gridpin_de_sources", path)
        if spec is None or spec.loader is None:
            raise BuildRefused(f"could not load {path}")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module


@dataclass(frozen=True)
class BuildPaths:
    code: pathlib.Path
    data: pathlib.Path
    common: pathlib.Path
    lock: pathlib.Path
    receipt: pathlib.Path
    evidence: pathlib.Path
    scratch: pathlib.Path
    extract_scratch: pathlib.Path
    export_scratch: pathlib.Path
    norm: pathlib.Path
    stats: pathlib.Path
    manifest: pathlib.Path
    export: pathlib.Path
    sheet: pathlib.Path


def _canonical_json_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode()


def _pretty_json_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()


def _write_all(fd: int, data: bytes) -> None:
    view = memoryview(data)
    while view:
        written = os.write(fd, view)
        if written <= 0:
            raise OSError(errno.EIO, "short write")
        view = view[written:]


def _fsync_dir(path: pathlib.Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _fd_is_regular_single(fd: int, label: str) -> os.stat_result:
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise BuildRefused(f"{label}: requires a regular single-link file")
    return info


def _path_is_regular_single(path: pathlib.Path, *, nonempty: bool = False) -> os.stat_result:
    try:
        info = path.lstat()
    except FileNotFoundError as exc:
        raise BuildRefused(f"required file was not created: {path}") from exc
    if path.is_symlink() or not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise BuildRefused(
            f"{path}: requires a regular single-link file; symlinks/hardlinks are forbidden"
        )
    if nonempty and info.st_size <= 0:
        raise BuildRefused(f"{path}: empty artifact")
    return info


def _exclusive_file(path: pathlib.Path, payload: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags, 0o600)
    except FileExistsError as exc:
        raise ReceiptExists(f"one-shot receipt already exists: {path}") from exc
    try:
        _fd_is_regular_single(fd, str(path))
        _write_all(fd, payload)
        os.fsync(fd)
    finally:
        os.close(fd)
    _fsync_dir(path.parent)


def _atomic_replace_json(path: pathlib.Path, payload: Mapping[str, Any], run_id: str) -> None:
    _path_is_regular_single(path, nonempty=True)
    tmp = path.with_name(f".{path.name}.{run_id}.{uuid.uuid4().hex}.tmp")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(tmp, flags, 0o600)
    try:
        _fd_is_regular_single(fd, str(tmp))
        _write_all(fd, _pretty_json_bytes(payload))
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(tmp, path)
    _fsync_dir(path.parent)
    _path_is_regular_single(path, nonempty=True)


def _stable_regular_bytes(path: pathlib.Path, max_bytes: int) -> tuple[bytes, dict[str, Any]]:
    """Read one named inode without links and prove it stayed stable while read.

    Recovery evidence includes an empty stdout log, so this deliberately accepts
    zero-byte files while retaining the upper bound and single-link checks.
    """
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags)
    except OSError as exc:
        raise BuildRefused(f"recovery evidence unavailable: {path}: {exc}") from exc
    try:
        before = _fd_is_regular_single(fd, str(path))
        if before.st_size < 0 or before.st_size > max_bytes:
            raise BuildRefused(
                f"{path}: size {before.st_size} outside the allowed 0..{max_bytes} bytes"
            )
        chunks: list[bytes] = []
        remaining = before.st_size
        while remaining:
            chunk = os.read(fd, min(1024 * 1024, remaining))
            if not chunk:
                raise BuildRefused(f"{path}: short read")
            chunks.append(chunk)
            remaining -= len(chunk)
        if os.read(fd, 1):
            raise BuildRefused(f"{path}: file grew during recovery snapshot")
        after = os.fstat(fd)
        if (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ):
            raise BuildRefused(f"{path}: file changed during recovery snapshot")
        try:
            named = path.lstat()
        except OSError as exc:
            raise BuildRefused(f"{path}: path disappeared during recovery snapshot") from exc
        if (named.st_dev, named.st_ino) != (before.st_dev, before.st_ino):
            raise BuildRefused(f"{path}: path was replaced during recovery snapshot")
        raw = b"".join(chunks)
        return raw, {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}
    finally:
        os.close(fd)


def _exclusive_evidence_copy(path: pathlib.Path, payload: bytes) -> dict[str, Any]:
    """Create an independent recovery copy; hard links and replacement are forbidden."""
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags, 0o600)
    except FileExistsError as exc:
        raise BuildRefused(f"recovery snapshot already exists: {path}") from exc
    try:
        info = _fd_is_regular_single(fd, str(path))
        _write_all(fd, payload)
        os.fsync(fd)
        if info.st_nlink != 1:
            raise BuildRefused(f"recovery snapshot is not independent: {path}")
    finally:
        os.close(fd)
    _fsync_dir(path.parent)
    copied, evidence = _stable_regular_bytes(path, _RECOVERY_COPY_LIMIT)
    if copied != payload:
        raise BuildRefused(f"recovery snapshot bytes mismatch: {path}")
    return evidence


@contextlib.contextmanager
def held_build_lock(common_dir: pathlib.Path) -> Iterator[pathlib.Path]:
    """Acquire the shared DE lock without following links and without waiting."""
    if not common_dir.is_absolute():
        raise BuildRefused(f"git common dir is not absolute: {common_dir}")
    common_info = common_dir.lstat()
    if common_dir.is_symlink() or not stat.S_ISDIR(common_info.st_mode):
        raise BuildRefused(f"git common dir is not a real directory: {common_dir}")
    lock = common_dir / f"{_STATE_STEM}.lock"
    flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(lock, flags, 0o600)
    except OSError as exc:
        raise BuildRefused(
            f"could not safely open lock {lock}: {exc}"
        ) from exc
    acquired = False
    try:
        opened = _fd_is_regular_single(fd, str(lock))
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            acquired = True
        except BlockingIOError as exc:
            raise BuildAlreadyRunning(
                "DE build is already running: shared flock is held"
            ) from exc
        named = lock.lstat()
        if (named.st_dev, named.st_ino) != (opened.st_dev, opened.st_ino):
            raise BuildRefused("lock path was replaced after open")
        if not stat.S_ISREG(named.st_mode) or named.st_nlink != 1:
            raise BuildRefused("lock path is no longer a regular single-link file")
        yield lock
    finally:
        if acquired:
            with contextlib.suppress(OSError):
                fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def _checked_capture(
    services: SystemServices,
    argv: Sequence[str],
    code_root: pathlib.Path,
) -> subprocess.CompletedProcess[Any]:
    result = services.run(
        list(argv), cwd=str(code_root), capture_output=True, text=True, shell=False
    )
    return result


def _git_text(services: SystemServices, code_root: pathlib.Path, args: Sequence[str]) -> str:
    argv = ["git", "-C", str(code_root), *args]
    result = _checked_capture(services, argv, code_root)
    if result.returncode != 0:
        detail = (result.stderr or result.stdout or "").strip()
        raise BuildRefused(f"git {' '.join(args)} failed: {detail[:300]}")
    return str(result.stdout).strip()


def _git_common_dir(services: SystemServices, code_root: pathlib.Path) -> pathlib.Path:
    raw = _git_text(
        services, code_root, ["rev-parse", "--path-format=absolute", "--git-common-dir"]
    )
    path = pathlib.Path(raw)
    if not path.is_absolute():
        raise BuildRefused(f"git returned a common dir that is not absolute: {raw!r}")
    try:
        return path.resolve(strict=True)
    except OSError as exc:
        raise BuildRefused(f"git common dir unavailable: {path}") from exc


def _validate_expected_sha(expected_sha: str) -> str:
    value = expected_sha.strip().lower()
    if _SHA40.fullmatch(value) is None:
        raise BuildRefused("--expected-sha must be a full 40-hex commit")
    return value


def _require_initial_clean_head(
    services: SystemServices, code_root: pathlib.Path, expected_sha: str
) -> None:
    head = _git_text(services, code_root, ["rev-parse", "--verify", "HEAD"]).lower()
    if head != expected_sha:
        raise BuildRefused(f"HEAD={head!r}, expected {expected_sha}")
    status_text = _git_text(
        services, code_root, ["status", "--porcelain=v1", "--untracked-files=no"]
    )
    if status_text:
        raise BuildRefused(
            "tracked/index tree is not clean; one-shot build requires a clean HEAD"
        )


def _require_recovery_base(
    services: SystemServices,
    code_root: pathlib.Path,
    expected_sha: str,
    *,
    base_sha: str | None = None,
) -> None:
    accepted_base = RECOVERY_BASE_SHA if base_sha is None else base_sha
    result = _checked_capture(
        services,
        [
            "git",
            "-C",
            str(code_root),
            "merge-base",
            "--is-ancestor",
            accepted_base,
            expected_sha,
        ],
        code_root,
    )
    if result.returncode != 0:
        raise BuildRefused(
            f"recovery HEAD {expected_sha} is not a descendant of the accepted fix {accepted_base}"
        )


def _require_git_unchanged(
    services: SystemServices, code_root: pathlib.Path, expected_sha: str
) -> None:
    head = _git_text(services, code_root, ["rev-parse", "--verify", "HEAD"]).lower()
    if head != expected_sha:
        raise BuildRefused("HEAD changed during one-shot build")
    for args in (
        ["diff", "--quiet", "--ignore-submodules", "--"],
        ["diff", "--cached", "--quiet", "--ignore-submodules", "--"],
    ):
        result = _checked_capture(services, ["git", "-C", str(code_root), *args], code_root)
        if result.returncode != 0:
            raise BuildRefused(
                "tracked/index state changed during one-shot build"
            )


def _free_bytes(services: SystemServices, data_dir: pathlib.Path) -> int:
    info = services.statvfs(str(data_dir))
    return int(info.f_bavail) * int(info.f_frsize)


def _require_free(
    services: SystemServices, data_dir: pathlib.Path, required: int, label: str
) -> int:
    free = _free_bytes(services, data_dir)
    if free < required:
        raise LowDisk(
            f"{label}: free {free / GIB:.3f} GiB, required {required / GIB:.3f} GiB"
        )
    return free


def _ensure_real_directory(path: pathlib.Path, label: str) -> pathlib.Path:
    resolved = path.resolve(strict=True)
    info = path.lstat()
    if path.is_symlink() or not stat.S_ISDIR(info.st_mode):
        raise BuildRefused(f"{label} is not a real directory: {path}")
    return resolved


def _make_paths(code_root: pathlib.Path, common: pathlib.Path, run_id: str) -> BuildPaths:
    if _RUN_ID.fullmatch(run_id) is None:
        raise BuildRefused("run id must be 32 lowercase hex")
    code = _ensure_real_directory(code_root, "code root")
    data = _ensure_real_directory(code / "data", "data dir")
    scratch = data / f".de-build-{run_id}"
    evidence = common / f"gridpin-de-build-evidence-{run_id}"
    return BuildPaths(
        code=code,
        data=data,
        common=common,
        lock=common / f"{_STATE_STEM}.lock",
        receipt=common / f"{_STATE_STEM}.receipt.json",
        evidence=evidence,
        scratch=scratch,
        extract_scratch=scratch / "extract",
        export_scratch=scratch / "export",
        norm=data / "de_norm.parquet",
        stats=data / "de_stats.json",
        manifest=data / "de_manifest.json",
        export=data / "build_de.csv.gz",
        sheet=data / "de.bin",
    )


def _make_recovery_paths(
    code_root: pathlib.Path, common: pathlib.Path, run_id: str
) -> BuildPaths:
    return replace(
        _make_paths(code_root, common, run_id),
        receipt=common / ATTEMPT2_RECEIPT_NAME,
    )


def _make_recovery3_paths(
    code_root: pathlib.Path, common: pathlib.Path, run_id: str
) -> BuildPaths:
    return replace(
        _make_paths(code_root, common, run_id),
        receipt=common / ATTEMPT3_RECEIPT_NAME,
    )


def _decode_json_object(raw: bytes, label: str) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BuildRefused(f"{label}: invalid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise BuildRefused(f"{label}: JSON root must be an object")
    return value


def _require_sha(evidence: Mapping[str, Any], expected: str, label: str) -> None:
    if evidence.get("sha256") != expected:
        raise BuildRefused(
            f"{label}: SHA-256={evidence.get('sha256')!r}, expected {expected}"
        )


def _recovery_snapshot_dir(paths: BuildPaths) -> pathlib.Path:
    return paths.common / ATTEMPT1_SNAPSHOT_NAME


def _recovery3_snapshot_dir(paths: BuildPaths) -> pathlib.Path:
    return paths.common / ATTEMPT2_SNAPSHOT_NAME


def _assert_recovery_outputs_state(paths: BuildPaths) -> None:
    """Accept only the known failed stats; every attempt-2 output must be fresh."""
    for path in (
        paths.norm,
        paths.manifest,
        paths.export,
        paths.sheet,
        paths.scratch,
        paths.evidence,
        _recovery_snapshot_dir(paths),
    ):
        try:
            path.lstat()
        except FileNotFoundError:
            continue
        raise BuildRefused(f"recovery output already exists (including symlinks): {path}")
    _path_is_regular_single(paths.stats, nonempty=True)


def _first_evidence_inventory(paths: BuildPaths) -> dict[str, Any]:
    evidence_dir = paths.common / f"gridpin-de-build-evidence-{FIRST_RUN_ID}"
    resolved = _ensure_real_directory(evidence_dir, "attempt-1 evidence dir")
    if resolved.parent != paths.common:
        raise BuildRefused("attempt-1 evidence dir escaped git common dir")
    actual = {entry.name for entry in evidence_dir.iterdir()}
    expected = set(FIRST_EVIDENCE_SHA256)
    if actual != expected:
        raise BuildRefused(
            "attempt-1 evidence inventory changed: "
            f"missing={sorted(expected - actual)}, extra={sorted(actual - expected)}"
        )
    files: dict[str, Any] = {}
    for name in sorted(expected):
        _raw, item = _stable_regular_bytes(evidence_dir / name, _RECOVERY_COPY_LIMIT)
        _require_sha(item, FIRST_EVIDENCE_SHA256[name], f"attempt-1 evidence/{name}")
        files[name] = item
    return {
        "path": str(evidence_dir),
        "files": files,
        "tree_sha256": hashlib.sha256(_canonical_json_bytes(files)).hexdigest(),
    }


def _verify_recovery_predecessor(
    paths: BuildPaths, *, require_failed_stats: bool
) -> dict[str, Any]:
    first_receipt = paths.common / f"{_STATE_STEM}.receipt.json"
    receipt_raw, receipt_ev = _stable_regular_bytes(first_receipt, _JSON_LIMIT)
    _require_sha(receipt_ev, FIRST_RECEIPT_SHA256, "attempt-1 receipt")
    receipt_obj = _decode_json_object(receipt_raw, "attempt-1 receipt")
    exact_fields = {
        "country": "DE",
        "source_release": DE_RELEASE,
        "expected_rows": EXPECTED_ROWS,
        "expected_sha": FIRST_BUILDER_SHA,
        "run_id": FIRST_RUN_ID,
        "status": "failed",
        "stages": [],
        "artifacts": {},
    }
    wrong = {
        key: {"expected": value, "actual": receipt_obj.get(key)}
        for key, value in exact_fields.items()
        if receipt_obj.get(key) != value
    }
    if wrong:
        raise BuildRefused(f"attempt-1 receipt identity mismatch: {wrong}")

    expected_evidence = paths.common / f"gridpin-de-build-evidence-{FIRST_RUN_ID}"
    try:
        declared_evidence = pathlib.Path(str(receipt_obj["evidence_dir"])).resolve(strict=True)
    except (KeyError, OSError) as exc:
        raise BuildRefused("attempt-1 receipt does not bind the original evidence dir") from exc
    if declared_evidence != expected_evidence.resolve(strict=True):
        raise BuildRefused("attempt-1 receipt evidence_dir does not match the pinned path")
    expected_scratch = paths.data / f".de-build-{FIRST_RUN_ID}"
    try:
        declared_scratch = pathlib.Path(str(receipt_obj["scratch_dir"]))
        resolved_scratch = _ensure_real_directory(
            declared_scratch, "attempt-1 scratch dir"
        )
    except (KeyError, OSError) as exc:
        raise BuildRefused("attempt-1 receipt does not bind the original scratch dir") from exc
    if resolved_scratch != expected_scratch.resolve(strict=True):
        raise BuildRefused(
            "attempt-1 receipt scratch_dir does not belong to the current original worktree"
        )

    evidence = _first_evidence_inventory(paths)
    auth_path = paths.code.parent / RECOVERY_AUTH_RELATIVE
    auth_raw, auth_ev = _stable_regular_bytes(auth_path, _RECOVERY_COPY_LIMIT)
    del auth_raw
    _require_sha(auth_ev, RECOVERY_AUTH_SHA256, "recovery authorization")

    stats_ev: dict[str, Any] = {
        "path": str(paths.stats),
        "sha256": FIRST_STATS_SHA256,
    }
    stats_raw: bytes | None = None
    if require_failed_stats:
        stats_raw, measured_stats = _stable_regular_bytes(paths.stats, _JSON_LIMIT)
        _require_sha(measured_stats, FIRST_STATS_SHA256, "attempt-1 failed stats")
        stats_obj = _decode_json_object(stats_raw, "attempt-1 failed stats")
        stats_exact = {
            "country": "DE",
            "release": DE_RELEASE,
            "status": "failed",
            "rows_src": EXPECTED_ROWS,
        }
        stats_wrong = {
            key: {"expected": value, "actual": stats_obj.get(key)}
            for key, value in stats_exact.items()
            if stats_obj.get(key) != value
        }
        if stats_wrong:
            raise BuildRefused(f"attempt-1 failed stats identity mismatch: {stats_wrong}")
        stats_ev = {"path": str(paths.stats), **measured_stats}

    return {
        "receipt_raw": receipt_raw,
        "stats_raw": stats_raw,
        "receipt": {"path": str(first_receipt), **receipt_ev},
        "stats": stats_ev,
        "evidence": evidence,
        "authorization": {"path": str(auth_path), **auth_ev},
        "first_receipt": receipt_obj,
    }


def _create_recovery_snapshot(
    paths: BuildPaths, predecessor: Mapping[str, Any]
) -> dict[str, Any]:
    snapshot = _recovery_snapshot_dir(paths)
    try:
        snapshot.mkdir(mode=0o700, parents=False, exist_ok=False)
    except FileExistsError as exc:
        raise BuildRefused(f"recovery snapshot already exists: {snapshot}") from exc
    _fsync_dir(snapshot.parent)
    evidence_copy = snapshot / "evidence"
    evidence_copy.mkdir(mode=0o700, parents=False, exist_ok=False)
    _fsync_dir(snapshot)

    receipt_raw = predecessor.get("receipt_raw")
    stats_raw = predecessor.get("stats_raw")
    if not isinstance(receipt_raw, bytes) or not isinstance(stats_raw, bytes):
        raise BuildRefused("recovery snapshot source bytes are not pinned")
    copies: dict[str, Any] = {}
    copies["attempt-1.receipt.json"] = _exclusive_evidence_copy(
        snapshot / "attempt-1.receipt.json", receipt_raw
    )
    # Compare only byte evidence; the source path intentionally differs.
    expected_receipt_copy = {
        key: predecessor["receipt"][key] for key in ("bytes", "sha256")
    }
    if copies["attempt-1.receipt.json"] != expected_receipt_copy:
        raise BuildRefused("recovery receipt copy does not match the authorized predecessor")
    copies["attempt-1.de_stats.json"] = _exclusive_evidence_copy(
        snapshot / "attempt-1.de_stats.json", stats_raw
    )
    expected_stats_copy = {
        key: predecessor["stats"][key] for key in ("bytes", "sha256")
    }
    if copies["attempt-1.de_stats.json"] != expected_stats_copy:
        raise BuildRefused("recovery stats copy does not match the authorized predecessor")
    first_evidence = pathlib.Path(str(predecessor["evidence"]["path"]))
    evidence_copies: dict[str, Any] = {}
    for name in sorted(FIRST_EVIDENCE_SHA256):
        raw, source_ev = _stable_regular_bytes(first_evidence / name, _RECOVERY_COPY_LIMIT)
        if source_ev != predecessor["evidence"]["files"][name]:
            raise BuildRefused(
                f"attempt-1 evidence changed before recovery snapshot: {name}"
            )
        copy_ev = _exclusive_evidence_copy(evidence_copy / name, raw)
        if copy_ev != source_ev:
            raise BuildRefused(f"recovery evidence copy mismatch: {name}")
        evidence_copies[name] = copy_ev

    source = {
        "receipt": dict(predecessor["receipt"]),
        "failed_stats": dict(predecessor["stats"]),
        "evidence": dict(predecessor["evidence"]),
    }
    manifest_obj = {
        "schema": 1,
        "kind": "gridpin-de-attempt-1-recovery-snapshot",
        "source_release": DE_RELEASE,
        "source": source,
        "copies": {
            **copies,
            "evidence": evidence_copies,
        },
    }
    manifest_bytes = _pretty_json_bytes(manifest_obj)
    manifest_ev = _exclusive_evidence_copy(snapshot / "manifest.json", manifest_bytes)
    return {
        "path": str(snapshot),
        "manifest": manifest_ev,
        "files": copies,
        "evidence_files": evidence_copies,
        "manifest_object": manifest_obj,
    }


def _verify_recovery_snapshot(snapshot_state: Mapping[str, Any]) -> None:
    snapshot = pathlib.Path(str(snapshot_state["path"]))
    _ensure_real_directory(snapshot, "recovery snapshot dir")
    actual_top = {entry.name for entry in snapshot.iterdir()}
    expected_top = {
        "attempt-1.receipt.json",
        "attempt-1.de_stats.json",
        "evidence",
        "manifest.json",
    }
    if actual_top != expected_top:
        raise BuildRefused("recovery snapshot inventory changed")
    for name, expected in snapshot_state["files"].items():
        _raw, measured = _stable_regular_bytes(snapshot / name, _RECOVERY_COPY_LIMIT)
        if measured != expected:
            raise BuildRefused(f"recovery snapshot changed: {name}")
    evidence_dir = _ensure_real_directory(snapshot / "evidence", "recovery evidence copy")
    if {entry.name for entry in evidence_dir.iterdir()} != set(FIRST_EVIDENCE_SHA256):
        raise BuildRefused("recovery evidence copy inventory changed")
    for name, expected in snapshot_state["evidence_files"].items():
        _raw, measured = _stable_regular_bytes(evidence_dir / name, _RECOVERY_COPY_LIMIT)
        if measured != expected:
            raise BuildRefused(f"recovery evidence copy changed: {name}")
    manifest_raw, measured_manifest = _stable_regular_bytes(
        snapshot / "manifest.json", _RECOVERY_COPY_LIMIT
    )
    if measured_manifest != snapshot_state["manifest"]:
        raise BuildRefused("recovery snapshot manifest changed")
    if _decode_json_object(manifest_raw, "recovery snapshot manifest") != snapshot_state[
        "manifest_object"
    ]:
        raise BuildRefused("recovery snapshot manifest lost source linkage")


def _verify_recovery_history(
    paths: BuildPaths,
    predecessor: Mapping[str, Any],
    snapshot_state: Mapping[str, Any],
) -> None:
    latest = _verify_recovery_predecessor(paths, require_failed_stats=False)
    for key in ("receipt", "evidence", "authorization"):
        if latest[key] != predecessor[key]:
            raise BuildRefused(f"attempt-1 {key} changed after recovery authorization")
    _verify_recovery_snapshot(snapshot_state)


def _assert_recovery3_outputs_state(paths: BuildPaths) -> None:
    """Accept only the pinned attempt-2 stats; every attempt-3 output is fresh."""
    for path in (
        paths.norm,
        paths.manifest,
        paths.export,
        paths.sheet,
        paths.scratch,
        paths.evidence,
        _recovery3_snapshot_dir(paths),
    ):
        try:
            path.lstat()
        except FileNotFoundError:
            continue
        raise BuildRefused(f"attempt-3 output already exists (including symlinks): {path}")
    _path_is_regular_single(paths.stats, nonempty=True)


def _second_evidence_inventory(paths: BuildPaths) -> dict[str, Any]:
    evidence_dir = paths.common / f"gridpin-de-build-evidence-{SECOND_RUN_ID}"
    resolved = _ensure_real_directory(evidence_dir, "attempt-2 evidence dir")
    if resolved.parent != paths.common:
        raise BuildRefused("attempt-2 evidence dir escaped git common dir")
    actual = {entry.name for entry in evidence_dir.iterdir()}
    expected = set(SECOND_EVIDENCE_SHA256)
    if actual != expected:
        raise BuildRefused(
            "attempt-2 evidence inventory changed: "
            f"missing={sorted(expected - actual)}, extra={sorted(actual - expected)}"
        )
    files: dict[str, Any] = {}
    for name in sorted(expected):
        _raw, item = _stable_regular_bytes(evidence_dir / name, _RECOVERY_COPY_LIMIT)
        _require_sha(item, SECOND_EVIDENCE_SHA256[name], f"attempt-2 evidence/{name}")
        files[name] = item
    tree_sha = hashlib.sha256(_canonical_json_bytes(files)).hexdigest()
    if tree_sha != SECOND_EVIDENCE_TREE_SHA256:
        raise BuildRefused("attempt-2 evidence tree SHA-256 does not match the pinned value")
    return {"path": str(evidence_dir), "files": files, "tree_sha256": tree_sha}


def _verify_attempt1_snapshot(
    paths: BuildPaths, attempt2_receipt: Mapping[str, Any]
) -> dict[str, Any]:
    declared = attempt2_receipt.get("snapshot")
    if not isinstance(declared, dict) or declared.get("status") != "complete":
        raise BuildRefused("attempt-2 receipt does not bind the complete snapshot of attempt 1")
    snapshot = _recovery_snapshot_dir(paths)
    resolved = _ensure_real_directory(snapshot, "attempt-1 snapshot dir")
    try:
        declared_path = pathlib.Path(str(declared["path"])).resolve(strict=True)
    except (KeyError, OSError) as exc:
        raise BuildRefused("attempt-1 snapshot path unavailable") from exc
    if declared_path != resolved:
        raise BuildRefused("attempt-1 snapshot path does not match the pinned value")
    expected_top = {
        "attempt-1.receipt.json",
        "attempt-1.de_stats.json",
        "evidence",
        "manifest.json",
    }
    if {entry.name for entry in snapshot.iterdir()} != expected_top:
        raise BuildRefused("attempt-1 snapshot inventory changed")

    expected_files = declared.get("files")
    expected_evidence = declared.get("evidence_files")
    if not isinstance(expected_files, dict) or not isinstance(expected_evidence, dict):
        raise BuildRefused("attempt-1 snapshot receipt inventory invalid")
    measured_files: dict[str, Any] = {}
    for name in ("attempt-1.receipt.json", "attempt-1.de_stats.json"):
        _raw, item = _stable_regular_bytes(snapshot / name, _RECOVERY_COPY_LIMIT)
        if item != expected_files.get(name):
            raise BuildRefused(f"attempt-1 snapshot changed: {name}")
        measured_files[name] = item
    evidence_dir = _ensure_real_directory(snapshot / "evidence", "attempt-1 snapshot evidence")
    if {entry.name for entry in evidence_dir.iterdir()} != set(FIRST_EVIDENCE_SHA256):
        raise BuildRefused("attempt-1 snapshot evidence inventory changed")
    measured_evidence: dict[str, Any] = {}
    for name in sorted(FIRST_EVIDENCE_SHA256):
        _raw, item = _stable_regular_bytes(evidence_dir / name, _RECOVERY_COPY_LIMIT)
        if item != expected_evidence.get(name):
            raise BuildRefused(f"attempt-1 snapshot evidence changed: {name}")
        measured_evidence[name] = item

    manifest_raw, manifest_ev = _stable_regular_bytes(
        snapshot / "manifest.json", _RECOVERY_COPY_LIMIT
    )
    if manifest_ev != declared.get("manifest"):
        raise BuildRefused("attempt-1 snapshot manifest changed")
    _require_sha(
        manifest_ev, ATTEMPT1_SNAPSHOT_MANIFEST_SHA256, "attempt-1 snapshot manifest"
    )
    recovery_of = attempt2_receipt.get("recovery_of")
    expected_manifest = {
        "schema": 1,
        "kind": "gridpin-de-attempt-1-recovery-snapshot",
        "source_release": DE_RELEASE,
        "source": {
            "receipt": recovery_of.get("receipt") if isinstance(recovery_of, dict) else None,
            "failed_stats": recovery_of.get("failed_stats") if isinstance(recovery_of, dict) else None,
            "evidence": recovery_of.get("evidence") if isinstance(recovery_of, dict) else None,
        },
        "copies": {**measured_files, "evidence": measured_evidence},
    }
    manifest_obj = _decode_json_object(manifest_raw, "attempt-1 snapshot manifest")
    if manifest_obj != expected_manifest:
        raise BuildRefused("attempt-1 snapshot manifest lost lineage")
    return {
        "path": str(snapshot),
        "manifest": manifest_ev,
        "files": measured_files,
        "evidence_files": measured_evidence,
        "manifest_object": manifest_obj,
    }


def _verify_attempt2_predecessor(
    paths: BuildPaths, *, require_stats: bool
) -> dict[str, Any]:
    attempt1 = _verify_recovery_predecessor(paths, require_failed_stats=False)
    receipt_path = paths.common / ATTEMPT2_RECEIPT_NAME
    receipt_raw, receipt_ev = _stable_regular_bytes(receipt_path, _JSON_LIMIT)
    _require_sha(receipt_ev, SECOND_RECEIPT_SHA256, "attempt-2 receipt")
    receipt_obj = _decode_json_object(receipt_raw, "attempt-2 receipt")
    exact_fields = {
        "schema": 2,
        "attempt": 2,
        "status": "failed",
        "country": "DE",
        "source_release": DE_RELEASE,
        "expected_rows": EXPECTED_ROWS,
        "expected_sha": SECOND_BUILDER_SHA,
        "run_id": SECOND_RUN_ID,
        "stages": [],
        "artifacts": {},
    }
    wrong = {
        key: {"expected": value, "actual": receipt_obj.get(key)}
        for key, value in exact_fields.items()
        if receipt_obj.get(key) != value
    }
    if wrong:
        raise BuildRefused(f"attempt-2 receipt identity mismatch: {wrong}")

    evidence = _second_evidence_inventory(paths)
    if receipt_obj.get("run_evidence") != evidence:
        raise BuildRefused("attempt-2 receipt run_evidence does not match the pinned evidence")
    expected_evidence_dir = pathlib.Path(str(evidence["path"])).resolve(strict=True)
    try:
        declared_evidence = pathlib.Path(str(receipt_obj["evidence_dir"])).resolve(strict=True)
    except (KeyError, OSError) as exc:
        raise BuildRefused("attempt-2 receipt does not bind the evidence dir") from exc
    if declared_evidence != expected_evidence_dir:
        raise BuildRefused("attempt-2 receipt evidence_dir does not match the pinned value")
    expected_scratch = paths.data / f".de-build-{SECOND_RUN_ID}"
    try:
        declared_scratch = _ensure_real_directory(
            pathlib.Path(str(receipt_obj["scratch_dir"])), "attempt-2 scratch dir"
        )
    except (KeyError, OSError) as exc:
        raise BuildRefused("attempt-2 receipt does not bind the scratch dir") from exc
    if declared_scratch != expected_scratch.resolve(strict=True):
        raise BuildRefused("attempt-2 receipt scratch_dir does not belong to the original worktree")

    recovery_of = receipt_obj.get("recovery_of")
    if not isinstance(recovery_of, dict):
        raise BuildRefused("attempt-2 receipt lost lineage of attempt 1")
    if (
        recovery_of.get("attempt") != 1
        or recovery_of.get("run_id") != FIRST_RUN_ID
        or recovery_of.get("builder_sha") != FIRST_BUILDER_SHA
        or recovery_of.get("receipt") != attempt1["receipt"]
        or recovery_of.get("evidence") != attempt1["evidence"]
        or recovery_of.get("failed_stats", {}).get("sha256") != FIRST_STATS_SHA256
    ):
        raise BuildRefused("attempt-2 receipt lineage of attempt 1 changed")
    if receipt_obj.get("authorization") != attempt1["authorization"]:
        raise BuildRefused("attempt-2 receipt authorization changed")
    attempt1_snapshot = _verify_attempt1_snapshot(paths, receipt_obj)

    auth_path = paths.code.parent / RECOVERY3_AUTH_RELATIVE
    _auth_raw, auth_ev = _stable_regular_bytes(auth_path, _RECOVERY_COPY_LIMIT)
    _require_sha(auth_ev, RECOVERY3_AUTH_SHA256, "attempt-3 authorization")

    stats_ev: dict[str, Any] = {"path": str(paths.stats), "sha256": SECOND_STATS_SHA256}
    stats_raw: bytes | None = None
    if require_stats:
        stats_raw, measured_stats = _stable_regular_bytes(paths.stats, _JSON_LIMIT)
        _require_sha(measured_stats, SECOND_STATS_SHA256, "attempt-2 stats witness")
        stats_obj = _decode_json_object(stats_raw, "attempt-2 stats witness")
        expected_raw_counts = [
            {"land_raw": code.removeprefix("DE-"), "rows": rows}
            for code, rows in sorted(DE_EXPECTED_LAND_ROWS.items())
        ]
        stats_exact = {
            "country": "DE",
            "release": DE_RELEASE,
            "status": "extract_validated",
            "rows_src": EXPECTED_ROWS,
            "coverage": DE_COVERAGE,
            "source_catalog_sha256": receipt_obj.get("source_catalog_sha256"),
            "raw_land_counts": expected_raw_counts,
        }
        stats_wrong = {
            key: {"expected": value, "actual": stats_obj.get(key)}
            for key, value in stats_exact.items()
            if stats_obj.get(key) != value
        }
        if stats_wrong:
            raise BuildRefused(f"attempt-2 stats witness identity mismatch: {stats_wrong}")
        stats_ev = {"path": str(paths.stats), **measured_stats}

    return {
        "receipt_raw": receipt_raw,
        "stats_raw": stats_raw,
        "receipt": {"path": str(receipt_path), **receipt_ev},
        "stats": stats_ev,
        "evidence": evidence,
        "authorization": {"path": str(auth_path), **auth_ev},
        "attempt2_authorization": dict(attempt1["authorization"]),
        "attempt1": {
            "receipt": dict(attempt1["receipt"]),
            "evidence": dict(attempt1["evidence"]),
            "authorization": dict(attempt1["authorization"]),
            "snapshot": attempt1_snapshot,
        },
        "second_receipt": receipt_obj,
    }


def _recovery3_snapshot_manifest_object(
    predecessor: Mapping[str, Any],
    copies: Mapping[str, Any],
    evidence_copies: Mapping[str, Any],
) -> dict[str, Any]:
    prior_snapshot = {
        key: value
        for key, value in predecessor["attempt1"]["snapshot"].items()
        if key != "manifest_object"
    }
    return {
        "schema": 1,
        "kind": "gridpin-de-attempt-2-recovery-snapshot",
        "source_release": DE_RELEASE,
        "source": {
            "receipt": dict(predecessor["receipt"]),
            "intermediate_stats": dict(predecessor["stats"]),
            "evidence": dict(predecessor["evidence"]),
            "attempt2_authorization": dict(predecessor["attempt2_authorization"]),
            "attempt1": {
                "receipt": dict(predecessor["attempt1"]["receipt"]),
                "evidence": dict(predecessor["attempt1"]["evidence"]),
                "snapshot": prior_snapshot,
            },
        },
        "copies": {**dict(copies), "evidence": dict(evidence_copies)},
    }


def _create_recovery3_snapshot(
    paths: BuildPaths, predecessor: Mapping[str, Any]
) -> dict[str, Any]:
    snapshot = _recovery3_snapshot_dir(paths)
    try:
        snapshot.mkdir(mode=0o700, parents=False, exist_ok=False)
    except FileExistsError as exc:
        raise BuildRefused(f"attempt-2 recovery snapshot already exists: {snapshot}") from exc
    _fsync_dir(snapshot.parent)
    evidence_copy = snapshot / "evidence"
    evidence_copy.mkdir(mode=0o700, parents=False, exist_ok=False)
    _fsync_dir(snapshot)

    receipt_raw = predecessor.get("receipt_raw")
    stats_raw = predecessor.get("stats_raw")
    if not isinstance(receipt_raw, bytes) or not isinstance(stats_raw, bytes):
        raise BuildRefused("attempt-2 snapshot source bytes are not pinned")
    copies = {
        "attempt-2.receipt.json": _exclusive_evidence_copy(
            snapshot / "attempt-2.receipt.json", receipt_raw
        ),
        "attempt-2.de_stats.json": _exclusive_evidence_copy(
            snapshot / "attempt-2.de_stats.json", stats_raw
        ),
    }
    for name, source in (
        ("attempt-2.receipt.json", predecessor["receipt"]),
        ("attempt-2.de_stats.json", predecessor["stats"]),
    ):
        if copies[name] != {key: source[key] for key in ("bytes", "sha256")}:
            raise BuildRefused(f"attempt-2 snapshot copy mismatch: {name}")

    evidence_source = pathlib.Path(str(predecessor["evidence"]["path"]))
    evidence_copies: dict[str, Any] = {}
    for name in sorted(SECOND_EVIDENCE_SHA256):
        raw, source_ev = _stable_regular_bytes(
            evidence_source / name, _RECOVERY_COPY_LIMIT
        )
        if source_ev != predecessor["evidence"]["files"][name]:
            raise BuildRefused(f"attempt-2 evidence changed before snapshot: {name}")
        copy_ev = _exclusive_evidence_copy(evidence_copy / name, raw)
        if copy_ev != source_ev:
            raise BuildRefused(f"attempt-2 evidence snapshot mismatch: {name}")
        evidence_copies[name] = copy_ev

    manifest_obj = _recovery3_snapshot_manifest_object(
        predecessor, copies, evidence_copies
    )
    manifest_ev = _exclusive_evidence_copy(
        snapshot / "manifest.json", _pretty_json_bytes(manifest_obj)
    )
    return {
        "path": str(snapshot),
        "manifest": manifest_ev,
        "files": copies,
        "evidence_files": evidence_copies,
        "manifest_object": manifest_obj,
    }


def _verify_recovery3_snapshot(
    snapshot_state: Mapping[str, Any], predecessor: Mapping[str, Any]
) -> None:
    snapshot = pathlib.Path(str(snapshot_state["path"]))
    _ensure_real_directory(snapshot, "attempt-2 recovery snapshot dir")
    expected_top = {
        "attempt-2.receipt.json",
        "attempt-2.de_stats.json",
        "evidence",
        "manifest.json",
    }
    if {entry.name for entry in snapshot.iterdir()} != expected_top:
        raise BuildRefused("attempt-2 recovery snapshot inventory changed")
    measured_files: dict[str, Any] = {}
    for name, expected in snapshot_state["files"].items():
        _raw, measured = _stable_regular_bytes(snapshot / name, _RECOVERY_COPY_LIMIT)
        if measured != expected:
            raise BuildRefused(f"attempt-2 recovery snapshot changed: {name}")
        measured_files[name] = measured
    evidence_dir = _ensure_real_directory(
        snapshot / "evidence", "attempt-2 recovery evidence copy"
    )
    if {entry.name for entry in evidence_dir.iterdir()} != set(SECOND_EVIDENCE_SHA256):
        raise BuildRefused("attempt-2 recovery evidence inventory changed")
    measured_evidence: dict[str, Any] = {}
    for name, expected in snapshot_state["evidence_files"].items():
        _raw, measured = _stable_regular_bytes(evidence_dir / name, _RECOVERY_COPY_LIMIT)
        if measured != expected:
            raise BuildRefused(f"attempt-2 recovery evidence changed: {name}")
        measured_evidence[name] = measured
    manifest_raw, measured_manifest = _stable_regular_bytes(
        snapshot / "manifest.json", _RECOVERY_COPY_LIMIT
    )
    if measured_manifest != snapshot_state["manifest"]:
        raise BuildRefused("attempt-2 recovery snapshot manifest changed")
    expected_manifest = _recovery3_snapshot_manifest_object(
        predecessor, measured_files, measured_evidence
    )
    if snapshot_state.get("manifest_object") != expected_manifest:
        raise BuildRefused("attempt-2 recovery snapshot state self-certification rejected")
    if _decode_json_object(
        manifest_raw, "attempt-2 recovery snapshot manifest"
    ) != expected_manifest:
        raise BuildRefused("attempt-2 recovery snapshot manifest lost lineage")


def _verify_recovery3_history(
    paths: BuildPaths,
    predecessor: Mapping[str, Any],
    snapshot_state: Mapping[str, Any],
) -> None:
    latest = _verify_attempt2_predecessor(paths, require_stats=False)
    for key in (
        "receipt",
        "evidence",
        "authorization",
        "attempt2_authorization",
        "attempt1",
    ):
        if latest[key] != predecessor[key]:
            raise BuildRefused(f"attempt-3 lineage {key} changed")
    _verify_recovery3_snapshot(snapshot_state, predecessor)


def _assert_outputs_absent(paths: BuildPaths) -> None:
    for path in (
        paths.norm,
        paths.stats,
        paths.manifest,
        paths.export,
        paths.sheet,
        paths.scratch,
        paths.evidence,
    ):
        try:
            path.lstat()
        except FileNotFoundError:
            continue
        raise BuildRefused(f"output already exists (including symlinks): {path}")


def _assert_receipt_absent(path: pathlib.Path) -> None:
    try:
        path.lstat()
    except FileNotFoundError:
        return
    raise ReceiptExists(f"one-shot receipt already exists: {path}")


def _prepare_run_dirs(paths: BuildPaths) -> None:
    for path in (paths.evidence, paths.scratch):
        path.mkdir(mode=0o700, parents=False, exist_ok=False)
        _fsync_dir(path.parent)
    paths.extract_scratch.mkdir(mode=0o700)
    paths.export_scratch.mkdir(mode=0o700)
    for child in (paths.scratch, paths.extract_scratch, paths.export_scratch):
        resolved = _ensure_real_directory(child, "run-scoped scratch")
        if paths.data not in resolved.parents:
            raise BuildRefused(f"scratch escaped data/: {resolved}")


def _manifest_plan(services: SystemServices, code_root: pathlib.Path) -> tuple[Any, str]:
    api = services.load_de_sources(code_root)
    try:
        manifest = api.build_manifest(DE_RELEASE)
        api.validate_manifest(manifest, DE_RELEASE)
    except (ValueError, SystemExit, TypeError, KeyError) as exc:
        raise BuildRefused(f"DE source manifest failed preflight: {exc}") from exc
    if not isinstance(manifest, dict):
        raise BuildRefused("build_manifest must return a dict")
    if str(manifest.get("country", "")).lower() != "de":
        raise BuildRefused("DE manifest does not declare country=de")
    if manifest.get("source_release") != DE_RELEASE:
        raise BuildRefused("DE manifest declares an incorrect source_release")
    try:
        catalog_sha = str(api.canonical_catalog_sha256())
    except (AttributeError, TypeError, ValueError) as exc:
        raise BuildRefused(f"de_sources did not return the canonical catalog hash: {exc}") from exc
    if not re.fullmatch(r"[0-9a-f]{64}", catalog_sha):
        raise BuildRefused("canonical DE source catalog hash must be 64 lowercase hex")
    if manifest.get("source_catalog_sha256") != catalog_sha:
        raise BuildRefused("DE manifest is not bound to the canonical source catalog hash")
    digest = hashlib.sha256(_canonical_json_bytes(manifest)).hexdigest()
    return manifest, digest


def _read_regular(path: pathlib.Path, max_bytes: int) -> bytes:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    try:
        info = _fd_is_regular_single(fd, str(path))
        if info.st_size <= 0 or info.st_size > max_bytes:
            raise BuildRefused(
                f"{path}: size {info.st_size} outside the allowed 1..{max_bytes} bytes"
            )
        chunks = []
        remaining = info.st_size
        while remaining:
            chunk = os.read(fd, min(1024 * 1024, remaining))
            if not chunk:
                raise BuildRefused(f"{path}: short read")
            chunks.append(chunk)
            remaining -= len(chunk)
        return b"".join(chunks)
    finally:
        os.close(fd)


def _file_evidence(path: pathlib.Path, max_bytes: int) -> dict[str, Any]:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    try:
        info = _fd_is_regular_single(fd, str(path))
        if info.st_size <= 0 or info.st_size > max_bytes:
            raise BuildRefused(
                f"{path}: size {info.st_size} outside the allowed 1..{max_bytes} bytes"
            )
        digest = hashlib.sha256()
        while True:
            chunk = os.read(fd, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
        return {"bytes": info.st_size, "sha256": digest.hexdigest()}
    finally:
        os.close(fd)


def _json_file(path: pathlib.Path) -> tuple[dict[str, Any], dict[str, Any]]:
    raw = _read_regular(path, _JSON_LIMIT)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BuildRefused(f"{path}: invalid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise BuildRefused(f"{path}: JSON root must be an object")
    return value, {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}


def _preflight_file(
    path: pathlib.Path,
    max_bytes: int,
    *,
    executable: bool = False,
) -> dict[str, Any]:
    """Hash one immutable local dependency without accepting links or empties."""
    info = _path_is_regular_single(path, nonempty=True)
    if executable and info.st_mode & 0o111 == 0:
        raise BuildRefused(f"local preflight: file is not executable: {path}")
    evidence = _file_evidence(path, max_bytes)
    if executable:
        evidence["executable"] = True
    return evidence


def _local_resource_evidence(paths: BuildPaths) -> dict[str, Any]:
    resources: dict[str, Any] = {}
    for relative in _REQUIRED_SCRIPTS:
        resources[relative] = _preflight_file(
            paths.code / relative, _PREFLIGHT_TEXT_MAX
        )
    for relative in _REQUIRED_MODELS:
        resources[relative] = _preflight_file(
            paths.code / relative, _PREFLIGHT_MODEL_MAX
        )
    resources[_GRIDPIN_BINARY] = _preflight_file(
        paths.code / _GRIDPIN_BINARY,
        _PREFLIGHT_BINARY_MAX,
        executable=True,
    )

    rules = paths.code / "rules"
    resolved_rules = _ensure_real_directory(rules, "local preflight rules dir")
    if resolved_rules.parent != paths.code:
        raise BuildRefused(f"local preflight: rules dir escaped code root: {resolved_rules}")
    required = set(_REQUIRED_RULE_TSV)
    actual = {entry.name for entry in rules.iterdir() if entry.name.endswith(".tsv")}
    missing = sorted(required - actual)
    extra = sorted(actual - required)
    if missing or extra:
        raise BuildRefused(
            "local preflight: rules/*.tsv set does not match the known 17 files; "
            f"missing={missing}, extra={extra}"
        )
    for name in _REQUIRED_RULE_TSV:
        relative = f"rules/{name}"
        resources[relative] = _preflight_file(
            paths.code / relative, _PREFLIGHT_TEXT_MAX
        )
    digest = hashlib.sha256(_canonical_json_bytes(resources)).hexdigest()
    return {"files": resources, "bundle_sha256": digest}


def _subprocess_env(additions: Mapping[str, str] | None = None) -> dict[str, str]:
    env = os.environ.copy()
    for name in _CONTROLLED_ENV:
        env.pop(name, None)
    if additions:
        env.update(additions)
    return env


def _local_capture(
    services: SystemServices,
    paths: BuildPaths,
    label: str,
    argv: Sequence[str],
    *,
    env: Mapping[str, str],
    cwd: pathlib.Path | None = None,
) -> subprocess.CompletedProcess[Any]:
    try:
        result = services.run(
            list(argv),
            cwd=str(cwd or paths.code),
            env=dict(env),
            capture_output=True,
            text=True,
            shell=False,
            timeout=_PREFLIGHT_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise BuildRefused(f"local preflight {label} could not start: {exc}") from exc
    if result.returncode != 0:
        detail = (result.stderr or result.stdout or "").strip()
        raise BuildRefused(
            f"local preflight {label} failed rc={result.returncode}: {detail[:500]}"
        )
    return result


def _json_stdout(result: subprocess.CompletedProcess[Any], label: str) -> dict[str, Any]:
    try:
        value = json.loads(str(result.stdout))
    except (TypeError, json.JSONDecodeError) as exc:
        raise BuildRefused(f"local preflight {label}: stdout is not a JSON object") from exc
    if not isinstance(value, dict):
        raise BuildRefused(f"local preflight {label}: stdout JSON root must be an object")
    return value


def _duckdb_local_preflight(
    services: SystemServices, paths: BuildPaths
) -> dict[str, Any]:
    argv = [sys.executable, "-I", "-c", _DUCKDB_PROBE]
    result = _local_capture(
        services,
        paths,
        "duckdb",
        argv,
        env=_subprocess_env({"PYTHONNOUSERSITE": "1"}),
    )
    evidence = _json_stdout(result, "duckdb")
    if evidence.get("package_version") != DUCKDB_VERSION:
        raise BuildRefused("local preflight duckdb: package version != 1.5.3")
    if evidence.get("engine_version") != DUCKDB_VERSION:
        raise BuildRefused("local preflight duckdb: engine version != 1.5.3")
    expected_settings = {
        "autoinstall_known_extensions": "false",
        "autoload_known_extensions": "false",
    }
    if evidence.get("settings") != expected_settings:
        raise BuildRefused("local preflight duckdb: automatic extension loading is not disabled")
    extensions = evidence.get("extensions")
    if not isinstance(extensions, dict) or set(extensions) != {"httpfs", "spatial"}:
        raise BuildRefused("local preflight duckdb: the exact httpfs+spatial set is missing")
    for name in ("httpfs", "spatial"):
        item = extensions.get(name)
        if not isinstance(item, dict) or item.get("loaded") is not True or item.get("installed") is not True:
            raise BuildRefused(f"local preflight duckdb: {name} was not proven to be locally loaded")
    return {
        **evidence,
        "python": sys.executable,
        "probe_sha256": hashlib.sha256(_DUCKDB_PROBE.encode()).hexdigest(),
    }


def _scratch_exclusive(path: pathlib.Path, payload: bytes) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags, 0o600)
    try:
        _fd_is_regular_single(fd, str(path))
        _write_all(fd, payload)
        os.fsync(fd)
    finally:
        os.close(fd)


def _mini_build_local_preflight(
    services: SystemServices,
    paths: BuildPaths,
    expected_sha: str,
    run_id: str,
) -> dict[str, Any]:
    manifest_obj = {
        "country": "de",
        "layer": "addresses",
        "license": "local-preflight-only",
        "source_release": DE_RELEASE,
    }
    manifest_bytes = _pretty_json_bytes(manifest_obj)
    binary = paths.code / _GRIDPIN_BINARY
    parser_model = paths.code / _REQUIRED_MODELS[0]
    rank_model = paths.code / _REQUIRED_MODELS[1]
    rules = paths.code / "rules"

    with tempfile.TemporaryDirectory(
        prefix=f".de-local-preflight-{run_id}-", dir=paths.data
    ) as temp_text:
        temp = pathlib.Path(temp_text)
        resolved_temp = _ensure_real_directory(temp, "local preflight scratch")
        if paths.data not in resolved_temp.parents:
            raise BuildRefused(f"local preflight scratch escaped data/: {resolved_temp}")
        csv_path = temp / "mini.csv"
        manifest_path = temp / "mini_manifest.json"
        sheet_path = temp / "mini.bin"
        _scratch_exclusive(csv_path, _MINI_CSV)
        _scratch_exclusive(manifest_path, manifest_bytes)

        build_argv = [
            str(binary),
            "build",
            str(csv_path),
            str(sheet_path),
            "--model",
            str(parser_model),
            "--rank",
            str(rank_model),
            "--rules",
            str(rules),
            "--meta",
            str(manifest_path),
        ]
        _local_capture(
            services,
            paths,
            "mini build",
            build_argv,
            env=_subprocess_env({"GRIDPIN_REQUIRE_META": "1"}),
            cwd=temp,
        )
        sheet_evidence = _file_evidence(sheet_path, _PREFLIGHT_SHEET_MAX)
        meta_argv = [str(binary), "meta", str(sheet_path), "--json"]
        meta_result = _local_capture(
            services,
            paths,
            "mini meta",
            meta_argv,
            env=_subprocess_env(),
            cwd=temp,
        )
        meta = _json_stdout(meta_result, "mini meta")
        expected_meta = {
            "builder_git": expected_sha,
            "country": "de",
            "layer": "addresses",
            "source_release": DE_RELEASE,
        }
        mismatched = {
            key: {"expected": value, "actual": meta.get(key)}
            for key, value in expected_meta.items()
            if meta.get(key) != value
        }
        if mismatched:
            raise BuildRefused(f"local preflight mini meta mismatch: {mismatched}")
        return {
            "csv": {
                "bytes": len(_MINI_CSV),
                "sha256": hashlib.sha256(_MINI_CSV).hexdigest(),
            },
            "manifest": {
                "bytes": len(manifest_bytes),
                "sha256": hashlib.sha256(manifest_bytes).hexdigest(),
            },
            "sheet": sheet_evidence,
            "meta": {key: meta[key] for key in expected_meta},
            "binary": str(binary),
            "models": [str(parser_model), str(rank_model)],
            "rules": str(rules),
        }


def _local_preflight(
    services: SystemServices,
    paths: BuildPaths,
    expected_sha: str,
    run_id: str,
) -> dict[str, Any]:
    """Prove the complete local toolchain before creating the one-shot receipt."""
    try:
        resources = _local_resource_evidence(paths)
        duckdb_evidence = _duckdb_local_preflight(services, paths)
        mini_build = _mini_build_local_preflight(
            services, paths, expected_sha, run_id
        )
        resources_after = _local_resource_evidence(paths)
        if resources_after != resources:
            raise BuildRefused(
                "local preflight: scripts/models/binary/rules changed during the check"
            )
    except BuildRefused:
        raise
    except (OSError, subprocess.SubprocessError) as exc:
        raise BuildRefused(f"local preflight failed closed: {exc}") from exc
    return {
        "resources": resources,
        "duckdb": duckdb_evidence,
        "mini_build": mini_build,
    }


def _validate_extract(
    services: SystemServices, paths: BuildPaths, planned_manifest: Mapping[str, Any]
) -> dict[str, Any]:
    norm = _file_evidence(paths.norm, NORM_BUDGET_BYTES)
    stats_obj, stats_ev = _json_file(paths.stats)
    manifest_obj, manifest_ev = _json_file(paths.manifest)
    if str(stats_obj.get("country", "")).upper() != "DE":
        raise BuildRefused("de_stats.json: country != DE")
    if stats_obj.get("release") != DE_RELEASE:
        raise BuildRefused("de_stats.json: release does not match the hard pin")
    if stats_obj.get("rows_src") != EXPECTED_ROWS:
        raise BuildRefused(
            f"de_stats.json: rows_src={stats_obj.get('rows_src')!r}, expected {EXPECTED_ROWS}"
        )
    kept = stats_obj.get("rows_kept")
    dropped = stats_obj.get("rows_dropped")
    if isinstance(kept, bool) or not isinstance(kept, int) or not 0 < kept <= EXPECTED_ROWS:
        raise BuildRefused("de_stats.json: rows_kept outside the allowed range")
    if isinstance(dropped, bool) or not isinstance(dropped, int) or dropped != EXPECTED_ROWS - kept:
        raise BuildRefused("de_stats.json: rows_dropped is inconsistent with rows_src/rows_kept")
    api = services.load_de_sources(paths.code)
    try:
        if hasattr(api, "validate_stats_witness"):
            api.validate_stats_witness(stats_obj, DE_RELEASE)
        api.validate_manifest(manifest_obj, DE_RELEASE)
    except (ValueError, SystemExit, TypeError, KeyError) as exc:
        raise BuildRefused(
            f"DE stats/manifest witness failed canonical validation: {exc}"
        ) from exc
    if manifest_obj != dict(planned_manifest):
        raise BuildRefused("de_manifest.json differs from the pre-receipt manifest plan")
    return {"de_norm.parquet": norm, "de_stats.json": stats_ev, "de_manifest.json": manifest_ev}


def _open_stage_log(path: pathlib.Path):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags, 0o600)
    try:
        _fd_is_regular_single(fd, str(path))
        # File fsync alone does not make its directory entry crash-durable.
        _fsync_dir(path.parent)
        return os.fdopen(fd, "wb", buffering=0)
    except BaseException:
        os.close(fd)
        raise


def _process_group_exists(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError as exc:
        raise StageFailed(
            f"process group {pgid} is not signalable by this build: {exc}"
        ) from exc
    return True


def _signal_process_group(pgid: int, signum: int) -> None:
    try:
        os.killpg(pgid, signum)
    except ProcessLookupError:
        pass
    except PermissionError as exc:
        raise StageFailed(
            f"process group {pgid} is not owned by this build: {exc}"
        ) from exc


def _stop_child(process: Any) -> None:
    """Stop the detached stage's entire process group before evidence sealing."""
    pid = getattr(process, "pid", None)
    if isinstance(pid, int) and pid > 0:
        if getattr(process, "_gridpin_parent_death_guard", False):
            # The cooperative wrapper owns the separately detached real-stage
            # group.  Address the unreaped wrapper PID, never probe/signal its
            # process-group id after poll()/wait() made that id reusable.
            if getattr(process, "returncode", None) is not None:
                return
            process.terminate()
            process.wait()
            return
        # start_new_session=True makes the stage leader its process-group id.
        _signal_process_group(pid, signal.SIGTERM)
        direct_reaped = False
        try:
            process.wait(timeout=10)
            direct_reaped = True
        except BaseException:
            pass
        # The leader may exit on TERM while a descendant ignores it and keeps
        # inherited evidence fds open.  Probe and kill the whole remaining group.
        if _process_group_exists(pid):
            _signal_process_group(pid, signal.SIGKILL)
        if not direct_reaped:
            # Do not release the shared flock until the direct child is reaped.
            process.wait()
        # A reaped leader does not imply that descendants closed inherited
        # evidence fds.  Fail-stop under the flock until the entire group is
        # gone; repeated SIGKILL also closes a narrow post-probe fork race.
        while _process_group_exists(pid):
            _signal_process_group(pid, signal.SIGKILL)
            time.sleep(0.01)
        return

    # Injectable unit-test processes have no OS pid; retain the same verified
    # TERM -> bounded wait -> KILL -> unbounded wait contract for them.
    with contextlib.suppress(Exception):
        process.terminate()
    try:
        process.wait(timeout=10)
    except BaseException:
        with contextlib.suppress(Exception):
            process.kill()
        # Do not release the shared flock until death is observed.  A bounded
        # post-kill wait could time out and leave the 32-GiB child running.
        process.wait()


def _terminate_for_low_disk(process: Any, stage: str) -> None:
    _stop_child(process)
    raise LowDisk(
        f"{stage}: free space fell below the non-negotiable 5 GiB floor; "
        "child stopped"
    )


def _run_child(
    services: SystemServices,
    paths: BuildPaths,
    stage: str,
    argv: Sequence[str],
    env_additions: Mapping[str, str],
    *,
    poll_interval: float,
) -> None:
    stdout_path = paths.evidence / f"{stage}.stdout.log"
    stderr_path = paths.evidence / f"{stage}.stderr.log"
    env = os.environ.copy()
    for name in _CONTROLLED_ENV:
        env.pop(name, None)
    env.update(env_additions)
    with _open_stage_log(stdout_path) as stdout, _open_stage_log(stderr_path) as stderr:
        guard_write_fd: int | None = None
        process: Any | None = None
        try:
            guarded_popen = getattr(services, "popen_guarded", None)
            if callable(guarded_popen):
                guard_read_fd, guard_write_fd = os.pipe()
                try:
                    process = guarded_popen(
                        list(argv),
                        guard_read_fd=guard_read_fd,
                        cwd=str(paths.code),
                        env=env,
                        stdout=stdout,
                        stderr=stderr,
                        shell=False,
                        start_new_session=True,
                    )
                finally:
                    os.close(guard_read_fd)
            else:
                # Injectable tests retain a direct fake process boundary.
                process = services.popen(
                    list(argv),
                    cwd=str(paths.code),
                    env=env,
                    stdout=stdout,
                    stderr=stderr,
                    shell=False,
                    start_new_session=True,
                )
            while True:
                if _free_bytes(services, paths.data) < FLOOR_BYTES:
                    _terminate_for_low_disk(process, stage)
                rc = process.poll()
                if rc is not None:
                    break
                services.sleep(poll_interval)
            pid = getattr(process, "pid", None)
            guarded = getattr(process, "_gridpin_parent_death_guard", False)
            if (
                not guarded
                and isinstance(pid, int)
                and pid > 0
                and _process_group_exists(pid)
            ):
                # A terminal leader with a live group means an illegal
                # background descendant.  Stop it on both rc=0 and rc!=0.
                _stop_child(process)
            os.fsync(stdout.fileno())
            os.fsync(stderr.fileno())
        except LowDisk:
            raise
        except BaseException:
            # statvfs/poll/sleep/fsync may fail after Popen.  Never release the
            # shared flock or seal a failed receipt while that child survives.
            if process is not None:
                _stop_child(process)
            raise
        finally:
            if guard_write_fd is not None:
                os.close(guard_write_fd)
    if rc != 0:
        raise StageFailed(f"{stage}: child exit={rc}; partial logs retained in {paths.evidence}")


def _append_event(
    paths: BuildPaths, event: Mapping[str, Any], *, create: bool = False
) -> None:
    path = paths.evidence / "events.jsonl"
    if create:
        fd = os.open(
            path,
            os.O_WRONLY
            | os.O_APPEND
            | os.O_CREAT
            | os.O_EXCL
            | getattr(os, "O_NOFOLLOW", 0),
            0o600,
        )
    else:
        fd = os.open(path, os.O_WRONLY | os.O_APPEND | getattr(os, "O_NOFOLLOW", 0))
    try:
        _fd_is_regular_single(fd, str(path))
        _write_all(fd, _canonical_json_bytes(dict(event)))
        os.fsync(fd)
    finally:
        os.close(fd)
    if create:
        _fsync_dir(path.parent)


def _run_evidence_inventory(
    paths: BuildPaths, *, require_complete: bool = False
) -> dict[str, Any]:
    evidence = _ensure_real_directory(paths.evidence, "attempt evidence dir")
    allowed = {"events.jsonl"}
    for stage, _argv, _env in _stage_argv(paths, "0" * 40):
        allowed.add(f"{stage}.stdout.log")
        allowed.add(f"{stage}.stderr.log")
    actual = {entry.name for entry in evidence.iterdir()}
    invalid_inventory = actual != allowed if require_complete else not actual.issubset(allowed)
    if "events.jsonl" not in actual or invalid_inventory:
        raise BuildRefused(
            "attempt evidence inventory is invalid: "
            f"missing={sorted(allowed - actual)}, extra={sorted(actual - allowed)}"
        )
    files: dict[str, Any] = {}
    for name in sorted(actual):
        _raw, item = _stable_regular_bytes(evidence / name, _RECOVERY_COPY_LIMIT)
        files[name] = item
    return {
        "path": str(evidence),
        "files": files,
        "tree_sha256": hashlib.sha256(_canonical_json_bytes(files)).hexdigest(),
    }


def _seal_success(
    paths: BuildPaths,
    receipt: dict[str, Any],
    run_id: str,
    artifacts: Mapping[str, Any],
    *,
    history_check: Callable[[], None] | None = None,
) -> None:
    if history_check is not None:
        history_check()
    _append_event(paths, {"event": "complete", "artifacts": dict(artifacts)})
    receipt["run_evidence"] = _run_evidence_inventory(paths, require_complete=True)
    receipt["status"] = "complete"
    if history_check is not None:
        # Close mutations during terminal event/inventory creation; keep this
        # immediately adjacent to the durable complete receipt replacement.
        history_check()
    _atomic_replace_json(paths.receipt, receipt, run_id)


def _seal_failure(
    paths: BuildPaths,
    receipt: dict[str, Any],
    run_id: str,
    exc: BaseException,
    *,
    failure_free: int | None = None,
) -> None:
    receipt["status"] = "failed"
    receipt["error"] = f"{type(exc).__name__}: {exc}"
    if failure_free is not None:
        receipt["failure_free_bytes"] = failure_free
    if paths.evidence.exists() and not paths.evidence.is_symlink():
        try:
            failed_event: dict[str, Any] = {
                "event": "failed",
                "error": receipt["error"],
            }
            if failure_free is not None:
                failed_event["free"] = failure_free
            _append_event(paths, failed_event)
            receipt["run_evidence"] = _run_evidence_inventory(paths)
        except Exception as evidence_exc:
            receipt["evidence_seal_error"] = (
                f"{type(evidence_exc).__name__}: {evidence_exc}"
            )
    with contextlib.suppress(Exception):
        _atomic_replace_json(paths.receipt, receipt, run_id)


def _receipt_payload(
    *,
    run_id: str,
    expected_sha: str,
    manifest_sha: str,
    source_catalog_sha: str,
    local_preflight: Mapping[str, Any],
    paths: BuildPaths,
) -> dict[str, Any]:
    return {
        "schema": 1,
        "status": "started",
        "country": "DE",
        "source_release": DE_RELEASE,
        "expected_rows": EXPECTED_ROWS,
        "expected_sha": expected_sha,
        "manifest_sha256": manifest_sha,
        "source_catalog_sha256": source_catalog_sha,
        "local_preflight": dict(local_preflight),
        "run_id": run_id,
        "evidence_dir": str(paths.evidence),
        "scratch_dir": str(paths.scratch),
        "disk_budget_bytes": {
            "floor": FLOOR_BYTES,
            "spill": SPILL_BYTES,
            "persistent": PERSISTENT_BUDGET_BYTES,
            "initial_required": INITIAL_REQUIRED_BYTES,
        },
        "stages": [],
        "artifacts": {},
    }


def _recovery_receipt_payload(
    *,
    run_id: str,
    expected_sha: str,
    manifest_sha: str,
    source_catalog_sha: str,
    local_preflight: Mapping[str, Any],
    predecessor: Mapping[str, Any],
    paths: BuildPaths,
    initial_free: int,
) -> dict[str, Any]:
    first_receipt = predecessor["first_receipt"]
    return {
        "schema": 2,
        "attempt": RECOVERY_ATTEMPT,
        "status": "started",
        "country": "DE",
        "source_release": DE_RELEASE,
        "expected_rows": EXPECTED_ROWS,
        "expected_sha": expected_sha,
        "recovery_base_sha": RECOVERY_BASE_SHA,
        "manifest_sha256": manifest_sha,
        "source_catalog_sha256": source_catalog_sha,
        "previous_source_catalog_sha256": first_receipt.get("source_catalog_sha256"),
        "local_preflight": dict(local_preflight),
        "run_id": run_id,
        "evidence_dir": str(paths.evidence),
        "scratch_dir": str(paths.scratch),
        "recovery_of": {
            "attempt": 1,
            "run_id": FIRST_RUN_ID,
            "builder_sha": FIRST_BUILDER_SHA,
            "receipt": dict(predecessor["receipt"]),
            "failed_stats": dict(predecessor["stats"]),
            "evidence": dict(predecessor["evidence"]),
        },
        "authorization": dict(predecessor["authorization"]),
        "snapshot": {
            "status": "planned",
            "path": str(_recovery_snapshot_dir(paths)),
        },
        "disk_budget_bytes": {
            "floor": FLOOR_BYTES,
            "spill": SPILL_BYTES,
            "persistent": PERSISTENT_BUDGET_BYTES,
            "initial_required": INITIAL_REQUIRED_BYTES,
            "initial_free": initial_free,
        },
        "stages": [],
        "artifacts": {},
    }


def _recovery3_receipt_payload(
    *,
    run_id: str,
    expected_sha: str,
    manifest_sha: str,
    source_catalog_sha: str,
    local_preflight: Mapping[str, Any],
    predecessor: Mapping[str, Any],
    paths: BuildPaths,
    initial_free: int,
) -> dict[str, Any]:
    second_receipt = predecessor["second_receipt"]
    attempt1 = predecessor["attempt1"]
    attempt1_snapshot = {
        key: value
        for key, value in attempt1["snapshot"].items()
        if key != "manifest_object"
    }
    return {
        "schema": 3,
        "attempt": RECOVERY3_ATTEMPT,
        "status": "started",
        "country": "DE",
        "source_release": DE_RELEASE,
        "expected_rows": EXPECTED_ROWS,
        "expected_sha": expected_sha,
        "recovery_base_sha": RECOVERY3_BASE_SHA,
        "manifest_sha256": manifest_sha,
        "source_catalog_sha256": source_catalog_sha,
        "previous_source_catalog_sha256": second_receipt.get("source_catalog_sha256"),
        "local_preflight": dict(local_preflight),
        "run_id": run_id,
        "evidence_dir": str(paths.evidence),
        "scratch_dir": str(paths.scratch),
        "recovery_of": {
            "attempt_1": {
                "attempt": 1,
                "run_id": FIRST_RUN_ID,
                "builder_sha": FIRST_BUILDER_SHA,
                "receipt": dict(attempt1["receipt"]),
                "evidence": dict(attempt1["evidence"]),
                "snapshot": attempt1_snapshot,
            },
            "attempt_2": {
                "attempt": 2,
                "run_id": SECOND_RUN_ID,
                "builder_sha": SECOND_BUILDER_SHA,
                "receipt": dict(predecessor["receipt"]),
                "intermediate_stats": dict(predecessor["stats"]),
                "evidence": dict(predecessor["evidence"]),
                "authorization": dict(predecessor["attempt2_authorization"]),
            },
        },
        "authorization": dict(predecessor["authorization"]),
        "snapshot": {
            "status": "planned",
            "path": str(_recovery3_snapshot_dir(paths)),
        },
        "disk_budget_bytes": {
            "floor": FLOOR_BYTES,
            "spill": ATTEMPT3_SPILL_BYTES,
            "persistent": PERSISTENT_BUDGET_BYTES,
            "initial_required": ATTEMPT3_INITIAL_REQUIRED_BYTES,
            "initial_free": initial_free,
        },
        "stages": [],
        "artifacts": {},
    }


def _stage_argv(
    paths: BuildPaths,
    expected_sha: str,
    *,
    spill_bytes: int = SPILL_BYTES,
) -> list[tuple[str, list[str], dict[str, str]]]:
    python = sys.executable
    return [
        (
            "extract",
            [python, "prep/overture.py", "DE", "--release", DE_RELEASE, "--offline-extensions"],
            {
                "GRIDPIN_DUCKDB_TEMP_DIR": str(paths.extract_scratch),
                "GRIDPIN_DUCKDB_MAX_TEMP_BYTES": str(spill_bytes),
            },
        ),
        (
            "export",
            [python, "prep/export_build.py", "data/de_norm.parquet", "data/build_de.csv.gz"],
            {
                "GRIDPIN_DUCKDB_TEMP_DIR": str(paths.export_scratch),
                "GRIDPIN_DUCKDB_MAX_TEMP_BYTES": str(spill_bytes),
            },
        ),
        (
            "build",
            [
                "gridpin/target/release/gridpin",
                "build",
                "data/build_de.csv.gz",
                "data/de.bin",
                "--model",
                "ml/parser_v0.bin",
                "--rank",
                "ml/rank_v0.bin",
                "--rules",
                "rules",
                "--meta",
                "data/de_manifest.json",
            ],
            {"GRIDPIN_REQUIRE_META": "1"},
        ),
        (
            "strict_gate",
            [
                python,
                "tools/sheet_meta_gate.py",
                "--strict",
                f"--expected-sha={expected_sha}",
                "data/de.bin",
            ],
            {},
        ),
    ]


def _execute_stages(
    services: SystemServices,
    paths: BuildPaths,
    expected_sha: str,
    planned_manifest: Mapping[str, Any],
    receipt: dict[str, Any],
    run_id: str,
    *,
    poll_interval: float,
    history_check: Callable[[], None] | None = None,
    spill_bytes: int = SPILL_BYTES,
) -> dict[str, Any]:
    commands = _stage_argv(paths, expected_sha, spill_bytes=spill_bytes)
    initial_required = FLOOR_BYTES + spill_bytes + PERSISTENT_BUDGET_BYTES
    before = {
        "extract": initial_required,
        "export": FLOOR_BYTES + spill_bytes + EXPORT_BUDGET_BYTES + BIN_BUDGET_BYTES,
        "build": FLOOR_BYTES + BIN_BUDGET_BYTES,
        "strict_gate": FLOOR_BYTES,
    }
    after = {
        "extract": before["export"],
        "export": before["build"],
        "build": FLOOR_BYTES,
        "strict_gate": FLOOR_BYTES,
    }
    artifacts: dict[str, Any] = {}
    for stage, argv, env in commands:
        if history_check is not None:
            history_check()
        _require_git_unchanged(services, paths.code, expected_sha)
        free_before = _require_free(services, paths.data, before[stage], f"before {stage}")
        _append_event(
            paths,
            {
                "event": "stage-start",
                "stage": stage,
                "argv": argv,
                "env": env,
                "free": free_before,
            },
        )
        _run_child(
            services,
            paths,
            stage,
            argv,
            env,
            poll_interval=poll_interval,
        )
        free_after = _require_free(services, paths.data, after[stage], f"after {stage}")
        if stage == "extract":
            artifacts.update(_validate_extract(services, paths, planned_manifest))
        elif stage == "export":
            artifacts["build_de.csv.gz"] = _file_evidence(
                paths.export, EXPORT_BUDGET_BYTES
            )
        elif stage == "build":
            artifacts["de.bin"] = _file_evidence(paths.sheet, BIN_BUDGET_BYTES)
        _require_git_unchanged(services, paths.code, expected_sha)
        if history_check is not None:
            history_check()
        receipt["stages"].append(stage)
        receipt["artifacts"] = artifacts
        _append_event(
            paths, {"event": "stage-complete", "stage": stage, "free": free_after}
        )
        _atomic_replace_json(paths.receipt, receipt, run_id)
    return artifacts


def run_once(
    expected_sha: str,
    *,
    code_root: pathlib.Path = CODE,
    common_dir: pathlib.Path | None = None,
    services: SystemServices | None = None,
    run_id: str | None = None,
    poll_interval: float = 1.0,
) -> pathlib.Path:
    """Execute the sealed build once and return the durable receipt path.

    ``common_dir`` and ``services`` exist for offline tests.  The command-line
    entry point always discovers the absolute shared Git common directory.
    """
    services = services or SystemServices()
    expected = _validate_expected_sha(expected_sha)
    code = pathlib.Path(code_root).resolve(strict=True)
    common = (
        pathlib.Path(common_dir).resolve(strict=True)
        if common_dir is not None
        else _git_common_dir(services, code)
    )
    chosen_run_id = run_id or uuid.uuid4().hex
    paths = _make_paths(code, common, chosen_run_id)

    with held_build_lock(common):
        _assert_receipt_absent(paths.receipt)
        _assert_outputs_absent(paths)
        _require_initial_clean_head(services, paths.code, expected)
        local_preflight = _local_preflight(
            services, paths, expected, chosen_run_id
        )
        _require_git_unchanged(services, paths.code, expected)
        planned_manifest, manifest_sha = _manifest_plan(services, paths.code)
        _require_free(services, paths.data, INITIAL_REQUIRED_BYTES, "initial preflight")

        receipt = _receipt_payload(
            run_id=chosen_run_id,
            expected_sha=expected,
            manifest_sha=manifest_sha,
            source_catalog_sha=str(planned_manifest["source_catalog_sha256"]),
            local_preflight=local_preflight,
            paths=paths,
        )
        _exclusive_file(paths.receipt, _pretty_json_bytes(receipt))
        try:
            _prepare_run_dirs(paths)
            _append_event(
                paths,
                {"event": "receipt-created", "run_id": chosen_run_id},
                create=True,
            )
            artifacts = _execute_stages(
                services,
                paths,
                expected,
                planned_manifest,
                receipt,
                chosen_run_id,
                poll_interval=poll_interval,
            )
            _seal_success(paths, receipt, chosen_run_id, artifacts)
            return paths.receipt
        except BaseException as exc:
            _seal_failure(paths, receipt, chosen_run_id, exc)
            raise


def run_recovery_once(
    expected_sha: str,
    *,
    code_root: pathlib.Path = CODE,
    common_dir: pathlib.Path | None = None,
    services: SystemServices | None = None,
    run_id: str | None = None,
    poll_interval: float = 1.0,
) -> pathlib.Path:
    """Execute the single owner-authorized attempt #2.

    This is not a general retry API.  The hard-coded authorization, predecessor
    hashes, fixed attempt number, distinct O_EXCL receipt, and original shared
    flock make any third invocation fail before examining outputs.
    """
    services = services or SystemServices()
    expected = _validate_expected_sha(expected_sha)
    code = pathlib.Path(code_root).resolve(strict=True)
    common = (
        pathlib.Path(common_dir).resolve(strict=True)
        if common_dir is not None
        else _git_common_dir(services, code)
    )
    chosen_run_id = run_id or uuid.uuid4().hex
    paths = _make_recovery_paths(code, common, chosen_run_id)

    with held_build_lock(common):
        # Mere existence, including a corrupt file or symlink, consumes attempt 2.
        _assert_receipt_absent(paths.receipt)
        _assert_recovery_outputs_state(paths)
        predecessor = _verify_recovery_predecessor(paths, require_failed_stats=True)
        _require_initial_clean_head(services, paths.code, expected)
        _require_recovery_base(services, paths.code, expected)
        local_preflight = _local_preflight(
            services, paths, expected, chosen_run_id
        )
        _require_git_unchanged(services, paths.code, expected)
        planned_manifest, manifest_sha = _manifest_plan(services, paths.code)
        initial_free = _require_free(
            services, paths.data, INITIAL_REQUIRED_BYTES, "recovery initial preflight"
        )

        # Re-read all owner authorization and failed evidence after the full
        # preflight, immediately before consuming the only recovery attempt.
        predecessor_after = _verify_recovery_predecessor(
            paths, require_failed_stats=True
        )
        for key in (
            "receipt_raw",
            "stats_raw",
            "receipt",
            "stats",
            "evidence",
            "authorization",
        ):
            if predecessor_after[key] != predecessor[key]:
                raise BuildRefused(f"attempt-1 {key} changed during recovery preflight")
        _require_git_unchanged(services, paths.code, expected)

        receipt = _recovery_receipt_payload(
            run_id=chosen_run_id,
            expected_sha=expected,
            manifest_sha=manifest_sha,
            source_catalog_sha=str(planned_manifest["source_catalog_sha256"]),
            local_preflight=local_preflight,
            predecessor=predecessor,
            paths=paths,
            initial_free=initial_free,
        )
        _exclusive_file(paths.receipt, _pretty_json_bytes(receipt))
        try:
            _prepare_run_dirs(paths)
            _append_event(
                paths,
                {
                    "event": "recovery-receipt-created",
                    "attempt": RECOVERY_ATTEMPT,
                    "run_id": chosen_run_id,
                    "recovery_of_run_id": FIRST_RUN_ID,
                },
                create=True,
            )
            snapshot_state = _create_recovery_snapshot(paths, predecessor)
            receipt["snapshot"] = {
                key: value
                for key, value in snapshot_state.items()
                if key != "manifest_object"
            }
            receipt["snapshot"]["status"] = "complete"
            _atomic_replace_json(paths.receipt, receipt, chosen_run_id)

            latest = _verify_recovery_predecessor(
                paths, require_failed_stats=True
            )
            for key in ("receipt", "stats", "evidence", "authorization"):
                if latest[key] != predecessor[key]:
                    raise BuildRefused(
                        f"attempt-1 {key} changed before the first recovery child"
                    )
            _verify_recovery_snapshot(snapshot_state)
            _append_event(
                paths,
                {
                    "event": "attempt-1-snapshot-complete",
                    "manifest_sha256": snapshot_state["manifest"]["sha256"],
                },
            )

            history_check = lambda: _verify_recovery_history(
                paths, predecessor, snapshot_state
            )
            artifacts = _execute_stages(
                services,
                paths,
                expected,
                planned_manifest,
                receipt,
                chosen_run_id,
                poll_interval=poll_interval,
                history_check=history_check,
            )
            _seal_success(
                paths,
                receipt,
                chosen_run_id,
                artifacts,
                history_check=history_check,
            )
            return paths.receipt
        except BaseException as exc:
            _seal_failure(paths, receipt, chosen_run_id, exc)
            raise


def run_recovery_attempt3_once(
    expected_sha: str,
    *,
    code_root: pathlib.Path = CODE,
    common_dir: pathlib.Path | None = None,
    services: SystemServices | None = None,
    run_id: str | None = None,
    poll_interval: float = 1.0,
) -> pathlib.Path:
    """Execute the single fixed owner-authorized attempt #3.

    This is deliberately separate from attempt #2: its own authorization,
    receipt, predecessor snapshot, and 32 GiB spill ceiling cannot be selected
    by a general attempt number or environment override.
    """
    services = services or SystemServices()
    expected = _validate_expected_sha(expected_sha)
    code = pathlib.Path(code_root).resolve(strict=True)
    common = (
        pathlib.Path(common_dir).resolve(strict=True)
        if common_dir is not None
        else _git_common_dir(services, code)
    )
    chosen_run_id = run_id or uuid.uuid4().hex
    paths = _make_recovery3_paths(code, common, chosen_run_id)

    with held_build_lock(common):
        # Any inode at this fixed path consumes attempt #3 and blocks attempt #4.
        _assert_receipt_absent(paths.receipt)
        _assert_recovery3_outputs_state(paths)
        predecessor = _verify_attempt2_predecessor(paths, require_stats=True)
        _require_initial_clean_head(services, paths.code, expected)
        _require_recovery_base(
            services, paths.code, expected, base_sha=RECOVERY3_BASE_SHA
        )
        local_preflight = _local_preflight(
            services, paths, expected, chosen_run_id
        )
        _require_git_unchanged(services, paths.code, expected)
        planned_manifest, manifest_sha = _manifest_plan(services, paths.code)
        initial_free = _require_free(
            services,
            paths.data,
            ATTEMPT3_INITIAL_REQUIRED_BYTES,
            "attempt-3 initial preflight",
        )

        predecessor_after = _verify_attempt2_predecessor(paths, require_stats=True)
        for key in (
            "receipt_raw",
            "stats_raw",
            "receipt",
            "stats",
            "evidence",
            "authorization",
            "attempt2_authorization",
            "attempt1",
        ):
            if predecessor_after[key] != predecessor[key]:
                raise BuildRefused(f"attempt-3 predecessor {key} changed during preflight")
        _require_git_unchanged(services, paths.code, expected)

        receipt = _recovery3_receipt_payload(
            run_id=chosen_run_id,
            expected_sha=expected,
            manifest_sha=manifest_sha,
            source_catalog_sha=str(planned_manifest["source_catalog_sha256"]),
            local_preflight=local_preflight,
            predecessor=predecessor,
            paths=paths,
            initial_free=initial_free,
        )
        _exclusive_file(paths.receipt, _pretty_json_bytes(receipt))
        try:
            _prepare_run_dirs(paths)
            _append_event(
                paths,
                {
                    "event": "recovery-receipt-created",
                    "attempt": RECOVERY3_ATTEMPT,
                    "run_id": chosen_run_id,
                    "recovery_of_run_ids": [FIRST_RUN_ID, SECOND_RUN_ID],
                    "spill_bytes": ATTEMPT3_SPILL_BYTES,
                },
                create=True,
            )
            snapshot_state = _create_recovery3_snapshot(paths, predecessor)
            receipt["snapshot"] = {
                key: value
                for key, value in snapshot_state.items()
                if key != "manifest_object"
            }
            receipt["snapshot"]["status"] = "complete"
            _atomic_replace_json(paths.receipt, receipt, chosen_run_id)

            latest = _verify_attempt2_predecessor(paths, require_stats=True)
            for key in (
                "receipt",
                "stats",
                "evidence",
                "authorization",
                "attempt2_authorization",
                "attempt1",
            ):
                if latest[key] != predecessor[key]:
                    raise BuildRefused(
                        f"attempt-3 predecessor {key} changed before the first child"
                    )
            _verify_recovery3_snapshot(snapshot_state, predecessor)
            _append_event(
                paths,
                {
                    "event": "attempt-2-snapshot-complete",
                    "manifest_sha256": snapshot_state["manifest"]["sha256"],
                },
            )

            history_check = lambda: _verify_recovery3_history(
                paths, predecessor, snapshot_state
            )
            try:
                artifacts = _execute_stages(
                    services,
                    paths,
                    expected,
                    planned_manifest,
                    receipt,
                    chosen_run_id,
                    poll_interval=poll_interval,
                    history_check=history_check,
                    spill_bytes=ATTEMPT3_SPILL_BYTES,
                )
            except BaseException:
                # A failing child must not hide a simultaneous history mutation.
                history_check()
                raise

            def final_check() -> None:
                # Bind terminal `complete` to the same exact tracked/index
                # state as every stage, including the small post-gate window.
                _require_git_unchanged(services, paths.code, expected)
                history_check()

            _seal_success(
                paths,
                receipt,
                chosen_run_id,
                artifacts,
                history_check=final_check,
            )
            return paths.receipt
        except BaseException as exc:
            failure_free: int | None = None
            try:
                failure_free = _free_bytes(services, paths.data)
            except Exception as disk_exc:
                receipt["failure_disk_error"] = (
                    f"{type(disk_exc).__name__}: {disk_exc}"
                )
            _seal_failure(
                paths,
                receipt,
                chosen_run_id,
                exc,
                failure_free=failure_free,
            )
            raise


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="one-shot guarded DE 2026-08-19.0 build")
    parser.add_argument("--expected-sha", required=True, help="exact clean 40-hex builder HEAD")
    recovery = parser.add_mutually_exclusive_group()
    recovery.add_argument(
        "--recovery-attempt-2",
        action="store_true",
        help="consume the single fixed owner-authorized DE recovery attempt",
    )
    recovery.add_argument(
        "--recovery-attempt-3",
        action="store_true",
        help="consume the single fixed owner-authorized DE recovery attempt #3",
    )
    args = parser.parse_args(argv)
    try:
        if args.recovery_attempt_2:
            receipt = run_recovery_once(args.expected_sha)
        elif args.recovery_attempt_3:
            receipt = run_recovery_attempt3_once(args.expected_sha)
        else:
            receipt = run_once(args.expected_sha)
    except BuildRefused as exc:
        parser.exit(1, f"STOP: {exc}\n")
    print(f"DE build complete; durable receipt: {receipt}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
