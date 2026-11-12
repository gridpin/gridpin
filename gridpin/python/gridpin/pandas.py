"""Geocode a pandas DataFrame column offline: `gridpin.geocode_df(df, "address", "us")`.

Needs pandas (`pip install "gridpin[pandas]"`). Importing this module also registers the
`df.gridpin` accessor: `df.gridpin.geocode("address", "us")`, `df.gridpin.reverse("us")`.

The answer columns are those `gridpin batch` adds to a CSV and `gp_geocode_df` adds in the R
package: gp_lat, gp_lon, gp_precision, gp_confidence, gp_weak, gp_street, gp_housenumber,
gp_postcode, gp_commune, gp_region, gp_match_level, gp_match_status, plus gp_rank when k > 1 (one row per candidate) and
gp_distance_m for reverse lookups. Every input row is kept, in order; a row without an address or
without an answer keeps empty answer columns, except gp_match_status, which says U (unmatched).
"""

from __future__ import annotations

import math
from typing import List, Mapping, Optional, Sequence, Union

try:
    import pandas as pd
except ImportError as e:  # pragma: no cover - exercised without pandas installed
    raise ImportError('gridpin.pandas needs pandas: pip install "gridpin[pandas]"') from e

from ._open import SheetLike, geocoder

__all__ = ["GP_COLUMNS", "geocode_df", "reverse_df"]

# Same names and order as GP_COLUMNS in the engine's CSV output (gridpin/src/tabular.rs).
GP_COLUMNS = ("gp_lat", "gp_lon", "gp_precision", "gp_confidence", "gp_weak", "gp_street",
              "gp_housenumber", "gp_postcode", "gp_commune", "gp_region", "gp_match_level", "gp_match_status")
_FIELDS = ("lat", "lon", "precision", "confidence", "weak", "street", "housenumber", "postcode",
           "commune", "region", "match_level", "match_status")
_FLOAT = {"gp_lat", "gp_lon", "gp_confidence", "gp_distance_m"}

# `extras="h3,codes,..."`: the columns `gridpin batch --with` adds to a CSV, in the same order
# (gridpin/src/extras.rs, With::columns); a test compares the two.
_LINKS = (("osm", "gp_osm_url"), ("google", "gp_google_url"), ("apple", "gp_apple_url"),
          ("mapillary", "gp_mapillary_url"), ("bing", "gp_bing_url"))
_CARD = (("release", "gp_sheet_release"), ("data_date", "gp_data_date"), ("license", "gp_license"),
         ("sources", "gp_sources"), ("sha256", "gp_sheet_sha256"))


def _extras_cells(extras: dict) -> dict:
    """{column: value} of one answer's `extras`, named as the CLI's CSV columns."""
    cells = {}
    for key in ("h3", "geohash", "placekey_where", "plus_code", "mgrs", "dms"):
        if key in extras:
            cells[f"gp_{key}"] = extras[key]
    if "street_point" in extras:
        point = extras["street_point"] or {}
        cells["gp_street_lat"] = point.get("lat")
        cells["gp_street_lon"] = point.get("lon")
        cells["gp_street_point_m"] = extras.get("street_point_m")
    if "links" in extras:
        cells.update({col: extras["links"].get(key) for key, col in _LINKS})
    if "provenance" in extras:
        cells.update({col: extras["provenance"].get(key) for key, col in _CARD})
    return cells


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


_FIELD_NAMES = ("street", "number", "city", "region", "postcode")


def _field_rows(df: "pd.DataFrame", column: Mapping[str, str]) -> List[Union[tuple, str]]:
    """Per row `(street, number, city, region, postcode)` for the engine, or a line when the street is blank.

    A row with a blank street still has what its other cells say, read as one line (as the CLI's --columns).
    """
    unknown = sorted(set(column) - set(_FIELD_NAMES))
    if unknown:
        raise ValueError(f"unknown fields {unknown}; use {', '.join(_FIELD_NAMES)}")
    if "street" not in column:
        raise ValueError('a mapping of columns needs "street"')
    missing = [n for n in column.values() if n not in df.columns]
    if missing:
        raise KeyError(f"no column {missing!r}; the frame has: {list(df.columns)}")

    def cells(field):
        if field not in column:
            return [None] * len(df)
        return [None if _blank(c) else str(c).strip() for c in df[column[field]]]

    rows = []
    for street, number, city, region, postcode in zip(*map(cells, _FIELD_NAMES)):
        if street:
            rows.append((street, number, city or "", region, postcode))
        else:
            rows.append(", ".join(p for p in (number, postcode, city, region) if p))
    return rows


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


def _frame(df: "pd.DataFrame", answers: List[list], k: int, extra: Sequence[str] = (),
           extras_cols: Sequence[str] = ()) -> "pd.DataFrame":
    """df with the answer columns: one row per input row (k == 1) or per candidate (k > 1)."""
    if extras_cols:
        answers = [[{**h, **_extras_cells(h.get("extras", {}))} for h in found] for found in answers]
        extra = (*extra, *extras_cols)
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
    fields = (*_FIELDS, *(e if e in extras_cols else e[len("gp_"):] for e in extra))
    for name, field in zip((*GP_COLUMNS, *extra), fields):
        # a row without an answer has status U, every other answer column empty (as the CLI)
        none = "U" if name == "gp_match_status" else None
        values = [none if h is None else h.get(field) for h in hits]
        if name in _FLOAT:
            out[name] = pd.Series(values, index=out.index, dtype="float64")
        elif name == "gp_weak":
            out[name] = pd.array(values, dtype="boolean")
        else:
            out[name] = pd.Series(values, index=out.index, dtype="object")
    if k > 1:
        out["gp_rank"] = pd.array(ranks, dtype="Int64")
    return out


def _extras_columns(extras: Optional[str]) -> tuple:
    """The columns of an `extras` spec, from the engine itself (a bad spec is a ValueError)."""
    if not extras:
        return ()
    from ._gridpin import _extras_columns as columns
    return tuple(columns(extras))


def geocode_df(df: "pd.DataFrame", column: Union[str, Sequence[str], Mapping[str, str]], sheet: SheetLike,
               k: int = 1, poi=None, extras: Optional[str] = None) -> "pd.DataFrame":
    """A copy of `df` with the answer columns for the addresses in `column`.

    column  one column name, or several joined with ", " ("street", "city", "postcode"), or a mapping
            {"street": "Street", "number": "No", "city": "Town", "region": "State", "postcode": "ZIP"} when the address is
            already split: the cells go to the engine as fields. "street" is required, the rest optional.
    sheet   a country code ("us"; the sheet must be downloaded: gridpin.sheets.fetch("us")),
            a path to a .bin file, or an open gridpin.Geocoder.
    k       candidates per address; with k > 1 one row per candidate and gp_rank numbers them.
    poi     an optional points-of-interest sheet opened with the address sheet.
    extras  extra answer fields as more columns, as `gridpin batch --with`: "h3,geohash,placekey,codes,
            street_point,links,provenance" (h3 and geohash take a level: "h3:8").
    """
    k = _k(k)
    g = geocoder(sheet, poi)
    if isinstance(column, Mapping):
        rows = _field_rows(df, column)
        fields = [r for r in rows if isinstance(r, tuple)]
        lines = [r for r in rows if isinstance(r, str) and r]
        by_fields = iter(g.geocode_fields_many(fields, k, extras=extras) if fields else [])
        by_lines = iter(g.geocode_many(lines, k, extras=extras) if lines else [])
        answers = [next(by_fields) if isinstance(r, tuple) else (next(by_lines) if r else []) for r in rows]
        return _frame(df, answers, k, extras_cols=_extras_columns(extras))
    queries = _queries(df, column)
    asked = [q for q in queries if q]
    found = iter(g.geocode_many(asked, k, extras=extras) if asked else [])
    answers = [next(found) if q else [] for q in queries]
    return _frame(df, answers, k, extras_cols=_extras_columns(extras))


def reverse_df(df: "pd.DataFrame", sheet: SheetLike, lat: str = "lat", lon: str = "lon",
               k: int = 1, extras: Optional[str] = None) -> "pd.DataFrame":
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
            answers.append(g.reverse(float(y), float(x), k, extras=extras))
        except ValueError as e:
            raise ValueError(f"row {label!r}: {e}") from None
    return _frame(df, answers, k, extra=("gp_distance_m",), extras_cols=_extras_columns(extras))


@pd.api.extensions.register_dataframe_accessor("gridpin")
class GridpinAccessor:
    """`df.gridpin.geocode("address", "us")` and `df.gridpin.reverse("us", lat="lat", lon="lon")`."""

    def __init__(self, df: "pd.DataFrame"):
        self._df = df

    def geocode(self, column: Union[str, Sequence[str]], sheet: SheetLike, k: int = 1,
                poi=None, extras: Optional[str] = None) -> "pd.DataFrame":
        return geocode_df(self._df, column, sheet, k=k, poi=poi, extras=extras)

    def reverse(self, sheet: SheetLike, lat: str = "lat", lon: str = "lon", k: int = 1,
                extras: Optional[str] = None) -> "pd.DataFrame":
        return reverse_df(self._df, sheet, lat=lat, lon=lon, k=k, extras=extras)
