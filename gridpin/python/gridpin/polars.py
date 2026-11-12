"""Geocode a polars DataFrame column offline: `df.gridpin.geocode("address", "us")`.

Needs polars (`pip install polars`). Importing this module registers the `gridpin` namespace on polars DataFrames;
the answer columns are those of `gridpin batch` (gp_lat, gp_lon, gp_precision, …, gp_match_status), one answer per
row, input order kept. Rows go to the engine in one batch (`Geocoder.geocode_many`).
"""

from __future__ import annotations

from typing import Optional

try:
    import polars as pl
except ImportError as e:  # pragma: no cover - exercised without polars installed
    raise ImportError("gridpin.polars needs polars: pip install polars") from e

from ._open import SheetLike, geocoder

__all__ = ["geocode_pl"]

GP_COLUMNS = ("gp_lat", "gp_lon", "gp_precision", "gp_confidence", "gp_weak", "gp_street",
              "gp_housenumber", "gp_postcode", "gp_commune", "gp_region", "gp_match_level", "gp_match_status")
_FIELDS = ("lat", "lon", "precision", "confidence", "weak", "street", "housenumber", "postcode",
           "commune", "region", "match_level", "match_status")


def geocode_pl(df: "pl.DataFrame", column: str, sheet: SheetLike, poi: Optional[str] = None) -> "pl.DataFrame":
    geo = geocoder(sheet, poi)
    texts = [v if isinstance(v, str) else "" for v in df.get_column(column).to_list()]
    answers = geo.geocode_many(texts, 1)
    cols = {c: [] for c in GP_COLUMNS}
    for hits in answers:
        top = hits[0] if hits else None
        for c, f in zip(GP_COLUMNS, _FIELDS):
            cols[c].append(None if top is None else top.get(f))
        if top is None:
            cols["gp_match_status"][-1] = "U"
    return df.with_columns([pl.Series(c, v, strict=False) for c, v in cols.items()])


@pl.api.register_dataframe_namespace("gridpin")
class GridPinNamespace:
    def __init__(self, df: "pl.DataFrame"):
        self._df = df

    def geocode(self, column: str, sheet: SheetLike, poi: Optional[str] = None) -> "pl.DataFrame":
        return geocode_pl(self._df, column, sheet, poi)
