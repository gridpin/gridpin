"""On local Spark, against the Monaco smoke sheet: the UDF answers what the engine answers, row for row."""
import pathlib

import pytest

pyspark = pytest.importorskip("pyspark")

import gridpin  # noqa: E402

from gridpin_spark import SPARK_SCHEMA, geocode, geocode_udf  # noqa: E402

SHEET = pathlib.Path(__file__).resolve().parents[3] / "data" / "mc.bin"
ADDRESS = "boulevard des moulins 27 monaco"


@pytest.fixture(scope="module")
def spark():
    from pyspark.sql import SparkSession

    session = (
        SparkSession.builder.master("local[2]").appName("gridpin-test")
        .config("spark.ui.enabled", "false").config("spark.sql.shuffle.partitions", "2").getOrCreate()
    )
    yield session
    session.stop()


def test_a_frame_gets_the_answer_columns_with_the_engine_answer_row_for_row(spark):
    rows = [(1, ADDRESS), (2, "xyzzy plugh nonexistent street 999"), (3, None), (4, "monaco")]
    df = spark.createDataFrame(rows, "id int, address string").repartition(2)
    out = {r["id"]: r for r in geocode(df, "address", str(SHEET)).collect()}
    assert sorted(out) == [1, 2, 3, 4], "every row stays"
    top = gridpin.Geocoder(str(SHEET)).geocode(ADDRESS, 1)[0]
    assert (out[1]["gp_lat"], out[1]["gp_lon"]) == (top["lat"], top["lon"])
    assert (out[1]["gp_street"], out[1]["gp_housenumber"]) == (top["street"], top["housenumber"])
    assert out[1]["gp_match_level"] == "PointAddress" and out[1]["gp_weak"] is False
    for empty in (2, 3):
        assert out[empty]["gp_lat"] is None and out[empty]["gp_match_status"] == "U"
    assert out[4]["gp_weak"] is True and out[4]["gp_match_status"] == "T"


def test_the_udf_is_usable_on_its_own_and_its_schema_names_the_columns(spark):
    df = spark.createDataFrame([(ADDRESS,)], "address string")
    answer = df.select(geocode_udf(str(SHEET))("address").alias("gp")).first()["gp"]
    assert answer["gp_precision"] == "house"
    assert SPARK_SCHEMA.split(", ")[0] == "gp_lat double"


def test_a_frame_with_answer_columns_is_refused(spark):
    df = spark.createDataFrame([(ADDRESS, 1.0)], "address string, gp_lat double")
    with pytest.raises(ValueError, match="gp_lat"):
        geocode(df, "address", str(SHEET))
