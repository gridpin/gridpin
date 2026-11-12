"""Data-quality checks that an address column geocodes: the share itself, a pandera check and the SQL of a Soda metric.

    import gridpin.dq
    gridpin.dq.share_at_level(df["address"], "fr", level="house")        # the share, 0..1
    pandera: pa.Column(str, checks=gridpin.dq.pandera_check("fr", level="house", at_least=0.95))
    Soda: `soda_check_sql(table, column)` gives the SQL of a user-defined metric over DuckDB with gridpin_ext.
    Any other tool (Great Expectations among them) can compare `share_at_level` with its own threshold.

Levels, from the most precise: house (PointAddress), street (StreetAddress … StreetName), locality.
A weak answer (`weak`: the engine itself says it needs checking) reaches no level: on the owner's points it is
right far less often than the rest, so counting it would pass a column that does not really geocode.
"""

from __future__ import annotations

from typing import Iterable, Sequence

from ._open import SheetLike, geocoder

__all__ = ["LEVELS", "reaches", "share_at_level", "pandera_check", "soda_check_sql"]

LEVELS = {
    "house": {"PointAddress"},
    "street": {"PointAddress", "StreetAddress", "StreetAddressExt", "StreetName", "StreetInt"},
    "locality": {"PointAddress", "StreetAddress", "StreetAddressExt", "StreetName", "StreetInt", "Postal", "Locality"},
}


def reaches(match_levels: Iterable[str], level: str) -> list:
    """For each answer's match_level (None: no answer), whether it reaches `level`."""
    if level not in LEVELS:
        raise ValueError(f"level {level!r}: one of {', '.join(LEVELS)}")
    return [m in LEVELS[level] for m in match_levels]


def _levels(values: Sequence, sheet: SheetLike) -> list:
    """The match_level of each value's answer; None for no answer and for a weak one."""
    texts = [v if isinstance(v, str) else "" for v in values]
    return [hits[0]["match_level"] if hits and not hits[0].get("weak") else None
            for hits in geocoder(sheet).geocode_many(texts, 1)]


def share_at_level(values: Sequence, sheet: SheetLike, level: str = "house") -> float:
    ok = reaches(_levels(list(values), sheet), level)
    return sum(ok) / len(ok) if ok else 1.0


def pandera_check(sheet: SheetLike, level: str = "house", at_least: float = 1.0):
    """A pandera Check: the share of the column reaching `level` is at least `at_least`."""
    import pandera as pa

    return pa.Check(lambda data: share_at_level(_column_values(data), sheet, level) >= at_least,
                    name=f"geocodes_to_{level}", error=f"fewer than {at_least:.0%} of the addresses reach {level}")


def _column_values(data) -> list:
    """The values a pandera check receives: a pandas Series, or `PolarsData(lazyframe, key)` under pandera.polars."""
    if hasattr(data, "lazyframe") and hasattr(data, "key"):
        return data.lazyframe.select(data.key).collect().to_series().to_list()
    return list(data)


def soda_check_sql(table: str, column: str, level: str = "house") -> str:
    """The SQL of a Soda user-defined metric (DuckDB with the gridpin_ext community extension loaded).

    Each address is geocoded once; a weak answer counts as not reaching the level, as in `share_at_level`."""
    if level not in LEVELS:
        raise ValueError(f"level {level!r}: one of {', '.join(LEVELS)}")
    levels = ", ".join(f"'{m}'" for m in sorted(LEVELS[level]))
    return (f"SELECT avg(CASE WHEN json_extract_string(g, '$.match_level') IN ({levels}) "
            f"AND coalesce(json_extract_string(g, '$.weak'), 'false') <> 'true' THEN 1.0 ELSE 0.0 END) "
            f"FROM (SELECT gridpin_geocode({column}) AS g FROM {table})")
