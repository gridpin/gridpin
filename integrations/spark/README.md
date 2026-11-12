# gridpin-spark

Geocode a Spark column offline: the engine and one sheet per country sit on the workers, and no address leaves the
cluster. No API key, no rate limit.

```python
from gridpin_spark import geocode

out = geocode(df, "address", "/Volumes/geo/sheets/usa.bin")
out.select("address", "gp_lat", "gp_lon", "gp_precision", "gp_match_level").show()
```

Answer columns are those of `gridpin batch`: `gp_lat`, `gp_lon`, `gp_precision`, `gp_confidence`, `gp_weak`, `gp_street`,
`gp_housenumber`, `gp_postcode`, `gp_commune`, `gp_region`, `gp_match_level`, `gp_match_status`. Every row stays; one
without an address or an answer has empty columns and `gp_match_status` `U`.

The sheet opens once per Python worker: the UDF is an iterator pandas UDF, so a batch is answered in one call. Without
this package, `gridpin.udf.geocode_iter` is the same function for your own `pandas_udf`.

## The sheet

Download a sheet with `gridpin sheets fetch us` on a machine that has network, and put the file where every worker can
read it. The file is opened with a memory map, so it needs a real path on the worker's file system.

## Databricks

| Compute | Works | Why |
|---|---|---|
| Classic cluster, DBR 14.3 LTS or later, sheet in a Unity Catalog volume | yes | volumes are visible from UDFs from this runtime on |
| Serverless | not supported | custom code has 1 GB of memory, less than the United States sheet (1.45 GB), and a Unity Catalog UDF has no file system to map a sheet from; smaller sheets have not been tried |

Install on the cluster from the repository (the package is not on PyPI):
`%pip install "gridpin-spark @ git+https://github.com/gridpin/gridpin@v0.9.0#subdirectory=integrations/spark"`, then the snippet above. Checked on local Spark, not on a live Databricks workspace.

## Terms

The code is Apache-2.0. A sheet carries the license and attribution of its sources: `gridpin meta usa.bin`.
