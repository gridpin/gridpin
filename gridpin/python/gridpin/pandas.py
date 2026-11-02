"""Geocode a pandas DataFrame column offline: `gridpin.geocode_df(df, "address", "us")`.

Needs pandas (`pip install "gridpin[pandas]"`). Importing this module also registers the
`df.gridpin` accessor: `df.gridpin.geocode("address", "us")`, `df.gridpin.reverse("us")`.

The answer columns are those `gridpin batch` adds to a CSV and `gp_geocode_df` adds in the R
package: gp_lat, gp_lon, gp_precision, gp_confidence, gp_weak, gp_street, gp_housenumber,
gp_postcode, gp_commune, gp_region, plus gp_rank when k > 1 (one row per candidate) and
gp_distance_m for reverse lookups. Every input row is kept, in order; a row without an address or
without an answer keeps empty answer columns.
"""

from __future__ import annotations

import math
from typing import List, Sequence, Union

try:
    import pandas as pd
except ImportError as e:  # pragma: no cover - exercised without pandas installed
    raise ImportError('gridpin.pandas needs pandas: pip install "gridpin[pandas]"') from e

from ._open import SheetLike, geocoder

__all__ = ["GP_COLUMNS", "geocode_df", "reverse_df"]

# Same names and order as GP_COLUMNS in the engine's CSV output (gridpin/src/tabular.rs).
GP_COLUMNS = ("gp_lat", "gp_lon", "gp_precision", "gp_confidence", "gp_weak", "gp_street",
              "gp_housenumber", "gp_postcode", "gp_commune", "gp_region")
_FIELDS = ("lat", "lon", "precision", "confidence", "weak", "street", "housenumber", "postcode",
           "commune", "region")
_FLOAT = {"gp_lat", "gp_lon", "gp_confidence", "gp_distance_m"}


def _k(k) -> int:
    if isinstance(k, bool) or not isinstance(k, int) or k < 1:
        raise ValueError("k must be a whole number of at least 1")
    return k


def _blank(cell) -> bool:
    if cell is None:
        return True
    if isinstance(cell, float) and math.isnan(cell):
        return True
    try:
        if pd.isna(cell):
            return True
    except (TypeError, ValueError):
        pass
    return not str(cell).strip()


def _queries(df: "pd.DataFrame", column: Union[str, Sequence[str]]) -> List[str]:
    """The query of every row: the chosen cells, trimmed, empty ones skipped, joined with ", " (as the CLI)."""
    names = [column] if isinstance(column, str) else list(column)
    if not names:
        raise ValueError("column is empty")
    missing = [n for n in names if n not in df.columns]
    if missing:
        raise KeyError(f"no column {missing!r}; the frame has: {list(df.columns)}")
    cells = df[names].itertuples(index=False, name=None)
    return [", ".join(str(c).strip() for c in row if not _blank(c)) for row in cells]


def _frame(df: "pd.DataFrame", answers: List[list], k: int, extra: Sequence[str] = ()) -> "pd.DataFrame":
    """df with the answer columns: one row per input row (k == 1) or per candidate (k > 1)."""
    clash = [c for c in (*GP_COLUMNS, *extra, "gp_rank") if c in df.columns]
    if clash:
        raise ValueError(f"the frame already has answer columns {clash}; drop them before geocoding again")
    positions, ranks, hits = [], [], []
    for i, found in enumerate(answers):
        found = found[:k]
        if not found:
            positions.append(i)
            ranks.append(None)
            hits.append(None)
        for r, h in enumerate(found, start=1):
            positions.append(i)
            ranks.append(r)
            hits.append(h)
    out = df.iloc[positions].copy()
    fields = (*_FIELDS, *(e[len("gp_"):] for e in extra))
    for name, field in zip((*GP_COLUMNS, *extra), fields):
        values = [None if h is None else h.get(field) for h in hits]
        if name in _FLOAT:
            out[name] = pd.Series(values, index=out.index, dtype="float64")
        elif name == "gp_weak":
            out[name] = pd.array(values, dtype="boolean")
        else:
            out[name] = pd.Series(values, index=out.index, dtype="object")
    if k > 1:
        out["gp_rank"] = pd.array(ranks, dtype="Int64")
    return out


def geocode_df(df: "pd.DataFrame", column: Union[str, Sequence[str]], sheet: SheetLike, k: int = 1,
               poi=None) -> "pd.DataFrame":
    """A copy of `df` with the answer columns for the addresses in `column`.

    column  one column name, or several joined with ", " ("street", "city", "postcode").
    sheet   a country code ("us"; the sheet must be downloaded: gridpin.sheets.fetch("us")),
            a path to a .bin file, or an open gridpin.Geocoder.
    k       candidates per address; with k > 1 one row per candidate and gp_rank numbers them.
    poi     an optional points-of-interest sheet opened with the address sheet.
    """
    k = _k(k)
    queries = _queries(df, column)
    g = geocoder(sheet, poi)
    asked = [q for q in queries if q]
    found = iter(g.geocode_many(asked, k) if asked else [])
    answers = [next(found) if q else [] for q in queries]
    return _frame(df, answers, k)


def reverse_df(df: "pd.DataFrame", sheet: SheetLike, lat: str = "lat", lon: str = "lon",
               k: int = 1) -> "pd.DataFrame":
    """A copy of `df` with the nearest address to each (lat, lon) point, plus gp_distance_m.

    A row with a missing coordinate keeps empty answer columns; a coordinate outside the valid
    range is a ValueError naming the row.
    """
    k = _k(k)
    for name in (lat, lon):
        if name not in df.columns:
            raise KeyError(f"no column {name!r}; the frame has: {list(df.columns)}")
    g = geocoder(sheet)
    answers = []
    for label, y, x in zip(df.index, df[lat], df[lon]):
        if _blank(y) or _blank(x):
            answers.append([])
            continue
        try:
            answers.append(g.reverse(float(y), float(x), k))
        except ValueError as e:
            raise ValueError(f"row {label!r}: {e}") from None
    return _frame(df, answers, k, extra=("gp_distance_m",))


@pd.api.extensions.register_dataframe_accessor("gridpin")
class GridpinAccessor:
    """`df.gridpin.geocode("address", "us")` and `df.gridpin.reverse("us", lat="lat", lon="lon")`."""

    def __init__(self, df: "pd.DataFrame"):
        self._df = df

    def geocode(self, column: Union[str, Sequence[str]], sheet: SheetLike, k: int = 1,
                poi=None) -> "pd.DataFrame":
        return geocode_df(self._df, column, sheet, k=k, poi=poi)

    def reverse(self, sheet: SheetLike, lat: str = "lat", lon: str = "lon", k: int = 1) -> "pd.DataFrame":
        return reverse_df(self._df, sheet, lat=lat, lon=lon, k=k)
