"""Geocoding inside a data platform's own Python workers: the sheet is a local file, no call leaves the machine.

    from gridpin.udf import geocode_iter, SPARK_SCHEMA
    geocode = pandas_udf(geocode_iter("/Volumes/geo/sheets/usa.bin"), SPARK_SCHEMA)
    df.withColumn("gp", geocode("address"))

`geocode_iter` returns a function from an iterator of pandas Series to an iterator of DataFrames, the shape of
Spark's iterator pandas UDF: the sheet opens once per Python worker and each batch is answered in one call.
A batch keeps its length and order; a row without an address or an answer has empty columns, gp_match_status U.
"""

from __future__ import annotations

from typing import Callable, Iterator

try:
    import pandas as pd
except ImportError as e:  # pragma: no cover - exercised without pandas installed
    raise ImportError('gridpin.udf needs pandas: pip install "gridpin[pandas]"') from e

from ._open import geocoder
from .pandas import GP_COLUMNS, _frame

__all__ = ["SPARK_SCHEMA", "geocode_iter"]

_TYPES = {"gp_lat": "double", "gp_lon": "double", "gp_confidence": "double", "gp_weak": "boolean"}

# The answer columns as a Spark DDL schema, in the order `geocode_iter` returns them.
SPARK_SCHEMA = ", ".join(f"{c} {_TYPES.get(c, 'string')}" for c in GP_COLUMNS)


def geocode_iter(sheet, poi=None) -> Callable[[Iterator["pd.Series"]], Iterator["pd.DataFrame"]]:
    """The batch function over one sheet (a path or a country code); nothing opens until the first batch."""

    def answer(batches: Iterator["pd.Series"]) -> Iterator["pd.DataFrame"]:
        g = geocoder(sheet, poi)
        for batch in batches:
            cells = batch.tolist()
            queries = ["" if pd.isna(c) else str(c).strip() for c in cells]
            asked = [q for q in queries if q]
            found = iter(g.geocode_many(asked, 1) if asked else [])
            answers = [next(found) if q else [] for q in queries]
            frame = pd.DataFrame(index=range(len(queries)))
            yield _frame(frame, answers, 1).reset_index(drop=True)

    return answer
