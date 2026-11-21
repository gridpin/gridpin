"""Resource-friendly DuckDB connection: avoid saturating the host machine.

By default DuckDB takes ALL cores and up to 80% of RAM, which can stall the UI
and other applications on a workstation. This module uses half the cores, a hard
memory cap, spills temporary files to disk, and disables insertion-order
preservation (lower memory use).

Override via environment variables: GRIDPIN_THREADS, GRIDPIN_MEM.  Heavy,
explicitly supervised runs may additionally isolate and cap their spill with
GRIDPIN_DUCKDB_TEMP_DIR and GRIDPIN_DUCKDB_MAX_TEMP_BYTES.

Without an explicit cap the spill is capped at the free space of the temp
directory's disk minus 8 GiB, read when the connection opens (ANSWERS §361 item 2):
a query that would spill past it fails instead of taking the disk below the
5 GiB stop line (PRINCIPLES 4.5; 22.10.2026 a join wrote 20 GB of spill).  With
8 GiB free or less the connection is refused before it opens.
"""
import os
import re
import shutil

import duckdb


_TEMP_DIR_ENV = "GRIDPIN_DUCKDB_TEMP_DIR"
_MAX_TEMP_BYTES_ENV = "GRIDPIN_DUCKDB_MAX_TEMP_BYTES"
_MAX_SETTING_BYTES = 2**63 - 1
_DISK_RESERVE_BYTES = 8 * 2**30


def _free_bytes(path: str) -> int:
    """Free bytes of the disk that holds `path`, read at its nearest existing ancestor."""
    probe = os.path.abspath(path)
    while not os.path.exists(probe):
        parent = os.path.dirname(probe)
        if parent == probe:
            break
        probe = parent
    return shutil.disk_usage(probe).free


def _default_max_temp_bytes(tmp: str) -> int:
    """The spill cap without an explicit one: free space minus 8 GiB; refuse at 8 GiB free or less."""
    free = _free_bytes(tmp)
    cap = free - _DISK_RESERVE_BYTES
    if cap < 1:
        raise RuntimeError(
            f"DuckDB: {free / 2**30:.1f} GiB free on the disk of {tmp}, not more than the 8 GiB reserve; "
            f"free space or set {_MAX_TEMP_BYTES_ENV} explicitly"
        )
    return cap


def _sql_string(value: str) -> str:
    """Return one quoted DuckDB string literal, without executable suffixes."""
    return "'" + value.replace("'", "''") + "'"


def _temp_directory(value: os.PathLike[str] | str | None) -> str:
    override = os.environ.get(_TEMP_DIR_ENV) if value is None else value
    if override is None:
        return os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "data", "tmp_duck"
        )
    try:
        raw = os.fspath(override)
    except TypeError as exc:
        raise TypeError("DuckDB temp directory must be a string or path-like value") from exc
    if not isinstance(raw, str):
        raise TypeError("DuckDB temp directory must be text, not bytes")
    if not raw or "\x00" in raw or not os.path.isabs(raw):
        raise ValueError("DuckDB temp directory override must be a non-empty absolute path")
    if os.path.normpath(raw) == os.path.sep:
        raise ValueError("DuckDB temp directory override must not be the filesystem root")
    return raw


def _max_temp_bytes(value: int | None) -> int | None:
    if value is None:
        raw = os.environ.get(_MAX_TEMP_BYTES_ENV)
        if raw is None:
            return None
        # ASCII canonical decimal only.  Parse first and render the SQL from the
        # resulting integer, so environment text can never become SQL syntax.
        if re.fullmatch(r"[1-9][0-9]*", raw) is None:
            raise ValueError(f"{_MAX_TEMP_BYTES_ENV} must be a positive decimal integer")
        parsed = int(raw)
    else:
        if isinstance(value, bool) or not isinstance(value, int):
            raise TypeError("max_temp_directory_size_bytes must be an integer byte count")
        parsed = value
    if not 1 <= parsed <= _MAX_SETTING_BYTES:
        raise ValueError(
            f"DuckDB max temp byte count must be between 1 and {_MAX_SETTING_BYTES}"
        )
    return parsed


def connect_tuned(
    *,
    temp_directory: os.PathLike[str] | str | None = None,
    max_temp_directory_size_bytes: int | None = None,
    disable_extension_autoload: bool = False,
) -> "duckdb.DuckDBPyConnection":
    # Resolve and validate all new, externally supplied values before opening a
    # connection or creating a directory.  Invalid guard input therefore fails
    # closed without beginning any DuckDB work.
    tmp = _temp_directory(temp_directory)
    max_temp_bytes = _max_temp_bytes(max_temp_directory_size_bytes)
    if max_temp_bytes is None:
        max_temp_bytes = _default_max_temp_bytes(tmp)
    threads = int(os.environ.get("GRIDPIN_THREADS", max(2, (os.cpu_count() or 4) // 2)))
    mem = os.environ.get("GRIDPIN_MEM", "3GB")
    con = duckdb.connect()
    if disable_extension_autoload:
        con.execute("SET autoinstall_known_extensions=false")
        con.execute("SET autoload_known_extensions=false")
    con.execute(f"SET threads TO {threads}")
    con.execute(f"SET memory_limit = '{mem}'")
    con.execute("SET preserve_insertion_order = false")
    os.makedirs(tmp, exist_ok=True)
    con.execute("SET temp_directory = " + _sql_string(tmp))
    con.execute(f"SET max_temp_directory_size='{max_temp_bytes}B'")
    return con

def cap_spill(con: "duckdb.DuckDBPyConnection") -> "duckdb.DuckDBPyConnection":
    """Cap the spill of a connection opened with `duckdb.connect()` the way `connect_tuned` does: an explicit
    GRIDPIN_DUCKDB_MAX_TEMP_BYTES, else free space minus 8 GiB on the disk of the connection's temp directory (as
    set at the call; a relative one is read from the current directory).  DuckDB's own default, 90 % of the free
    space, leaves 4.6 GiB of 46 — under the 5 GiB stop line.  Call it after the connection's own SET temp_directory."""
    tmp = con.execute("SELECT current_setting('temp_directory')").fetchone()[0] or ".tmp"
    tmp = os.path.abspath(tmp)
    cap = _max_temp_bytes(None)
    if cap is None:
        cap = _default_max_temp_bytes(tmp)
    con.execute(f"SET max_temp_directory_size='{cap}B'")
    return con


# Single source for the pinned Overture release. Monthly step X+7:
# check docs.overturemaps.org/release-calendar and bump BEFORE rebuilding sheets.
OVERTURE_RELEASE = "2026-06-17.0"
