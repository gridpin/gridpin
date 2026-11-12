"""Offline geocoding in Spark and Databricks: `geocode(df, "address", "/Volumes/geo/sheets/usa.bin")`."""

from __future__ import annotations

from gridpin.udf import SPARK_SCHEMA, geocode_iter

__all__ = ["SPARK_SCHEMA", "geocode", "geocode_udf"]


def geocode_udf(sheet, poi=None):
    """An iterator pandas UDF over one sheet file: the file must be readable from every worker."""
    from pyspark.sql.functions import pandas_udf

    return pandas_udf(geocode_iter(sheet, poi), SPARK_SCHEMA)


def geocode(df, column, sheet, poi=None):
    """`df` with the answer columns (gp_lat, gp_lon, gp_precision, ...) of the addresses in `column`."""
    from pyspark.sql.functions import col

    taken = [c for c in SPARK_SCHEMA.split(", ") if c.split()[0] in df.columns]
    if taken:
        raise ValueError(f"the frame already has answer columns {taken}; drop them before geocoding again")
    answer = "__gridpin_answer"
    with_answer = df.withColumn(answer, geocode_udf(sheet, poi)(col(column)))
    names = [c.split()[0] for c in SPARK_SCHEMA.split(", ")]
    return with_answer.select(*df.columns, *[col(f"{answer}.{n}").alias(n) for n in names])
