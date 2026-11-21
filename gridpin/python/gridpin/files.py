"""A CSV file in, the same CSV with the answer columns out: what `gridpin batch sheet in.csv out.csv --column ...` writes,
done by the Python package so a pipeline step needs no CLI. The orchestrator packages (Airflow, Dagster, Prefect) call
this one function; it returns a receipt of the run for the step's log or metadata.

    from gridpin.files import geocode_file
    receipt = geocode_file("orders.csv", "orders_geo.csv", "us", column="address")
"""
from __future__ import annotations

import csv
import os
import struct
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Union

from ._open import SheetLike, geocoder

__all__ = ["GP_COLUMNS", "geocode_file"]

# Same names and order as the engine's CSV output (gridpin/src/tabular.rs, GP_COLUMNS).
GP_COLUMNS = ("gp_lat", "gp_lon", "gp_precision", "gp_confidence", "gp_weak", "gp_street",
              "gp_housenumber", "gp_postcode", "gp_commune", "gp_region", "gp_match_level", "gp_match_status")
_UNMATCHED = "U"
_BATCH = 10_000


def _number(x: Any) -> str:
    """A float written as the CLI writes it (Rust's shortest form): 7.0 -> "7", 1e-05 -> "0.00001"."""
    s = repr(float(x))
    if "e" in s or "E" in s:
        s = format(float(x), "f").rstrip("0").rstrip(".") or "0"
    return s[:-2] if s.endswith(".0") else s


def _number32(x: Any) -> str:
    """The confidence is a 32-bit float in the engine: the CLI writes its shortest form ("0.84", not 0.8399999737…)."""
    want = struct.unpack("f", struct.pack("f", float(x)))[0]
    for digits in range(1, 10):
        s = f"{want:.{digits}g}"
        if struct.unpack("f", struct.pack("f", float(s)))[0] == want:
            return _number(float(s))
    return _number(want)


def _cells(hit: Optional[Dict[str, Any]]) -> List[str]:
    if hit is None:
        return [""] * (len(GP_COLUMNS) - 1) + [_UNMATCHED]
    return [_number(hit["lat"]), _number(hit["lon"]), str(hit["precision"]), _number32(hit["confidence"]),
            "true" if hit.get("weak") else "false", hit.get("street") or "", hit.get("housenumber") or "",
            hit.get("postcode") or "", hit.get("commune") or "", hit.get("region") or "",
            str(hit.get("match_level") or ""), str(hit.get("match_status") or "")]


def _columns(header: Sequence[str], column: Union[str, Sequence[str]]) -> List[int]:
    names = [column] if isinstance(column, str) else list(column)
    if not names:
        raise ValueError("name the address column(s)")
    lower = [h.lower() for h in header]
    out = []
    for n in names:
        if n.lower() not in lower:
            raise KeyError(f"no column {n!r}; the file has: {list(header)}")
        out.append(lower.index(n.lower()))
    return out


def _line(row: Sequence[str], cols: Sequence[int]) -> str:
    """The question of a row: the chosen cells, trimmed, empty ones skipped, joined with ", " (as the CLI does)."""
    return ", ".join(c for c in (row[i].strip() if i < len(row) else "" for i in cols) if c)


def geocode_file(input: Union[str, "os.PathLike[str]"], output: Union[str, "os.PathLike[str]"], sheet: SheetLike,
                 column: Union[str, Sequence[str]] = "address", poi: Optional[str] = None) -> Dict[str, Any]:
    """Geocode the address column(s) of a CSV file into a new CSV: every input row and column, then the `gp_*`
    columns of its best answer (several columns are joined with ", ", like the CLI). The output is written to a
    temporary file next to it and renamed at the end, so a failed run leaves no half file.

    Returns a receipt: rows, answered, weak, without_address, seconds, input, output, sheet."""
    src, dst = Path(input), Path(output)
    if src.resolve() == dst.resolve():
        raise ValueError("input and output are the same file")
    started = time.monotonic()
    g = geocoder(sheet, poi)
    with src.open(newline="", encoding="utf-8") as f:
        reader = csv.reader(f)
        try:
            header = next(reader)
        except StopIteration:
            raise ValueError(f"{src} is empty: a CSV with a header row is needed") from None
        taken = [h for h in header if h.lower() in GP_COLUMNS]
        if taken:
            raise ValueError(f"the input already has a {taken[0]!r} column (output of an earlier run?)")
        cols = _columns(header, column)
        tmp = dst.with_name(f".{dst.name}.{os.getpid()}.part")
        counts = {"rows": 0, "answered": 0, "weak": 0, "without_address": 0}
        try:
            with tmp.open("w", newline="", encoding="utf-8") as out:
                writer = csv.writer(out, lineterminator="\n")
                writer.writerow(list(header) + list(GP_COLUMNS))
                while True:
                    rows = [r for _, r in zip(range(_BATCH), reader)]
                    if not rows:
                        break
                    lines = [_line(r, cols) for r in rows]
                    asked = [q for q in lines if q]
                    found = iter(g.geocode_many(asked, 1) if asked else [])
                    for row, line in zip(rows, lines):
                        hit = (next(found) or [None])[0] if line else None
                        counts["rows"] += 1
                        if not line:
                            counts["without_address"] += 1
                        elif hit is not None:
                            counts["answered"] += 1
                            counts["weak"] += bool(hit.get("weak"))
                        writer.writerow(row + _cells(hit))
            os.replace(tmp, dst)
        finally:
            if tmp.exists():
                tmp.unlink()
    return {**counts, "seconds": round(time.monotonic() - started, 3), "input": str(src), "output": str(dst),
            "sheet": sheet if isinstance(sheet, str) else os.fspath(sheet) if isinstance(sheet, os.PathLike) else "Geocoder"}
