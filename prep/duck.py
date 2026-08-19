"""Resource-friendly DuckDB connection: avoid saturating the host machine.

By default DuckDB takes ALL cores and up to 80% of RAM, which can stall the UI
and other applications on a workstation. This module uses half the cores, a hard
memory cap, spills temporary files to disk, and disables insertion-order
preservation (lower memory use).

Override via environment variables: GRIDPIN_THREADS, GRIDPIN_MEM.
"""
import os

import duckdb


def connect_tuned() -> "duckdb.DuckDBPyConnection":
    threads = int(os.environ.get("GRIDPIN_THREADS", max(2, (os.cpu_count() or 4) // 2)))
    mem = os.environ.get("GRIDPIN_MEM", "3GB")
    con = duckdb.connect()
    con.execute(f"SET threads TO {threads}")
    con.execute(f"SET memory_limit = '{mem}'")
    con.execute("SET preserve_insertion_order = false")
    tmp = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "data", "tmp_duck")
    os.makedirs(tmp, exist_ok=True)
    con.execute(f"SET temp_directory = '{tmp}'")
    return con

# Single source for the pinned Overture release. Monthly step X+7:
# check docs.overturemaps.org/release-calendar and bump BEFORE rebuilding sheets.
OVERTURE_RELEASE = "2026-06-17.0"
