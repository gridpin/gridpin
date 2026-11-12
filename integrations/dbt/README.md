# GridPin for dbt

Macros `gridpin_geocode(column)`, `gridpin_lat`, `gridpin_lon`, `gridpin_level` for dbt-duckdb (through the DuckDB
community extension `gridpin_ext`, DuckDB 1.5.6+; older DuckDB versions install an older build) and Postgres (through `integrations/postgres/gridpin.sql`).

```yaml
# packages.yml
packages:
  - git: "https://github.com/gridpin/gridpin.git"
    subdirectory: "integrations/dbt"
# dbt_project.yml (dbt-duckdb)
on-run-start:
  - "INSTALL gridpin_ext FROM community; LOAD gridpin_ext; SELECT gridpin_load('/data/fr.bin')"
```

`models/example_geocoded.sql` shows a model. Licence: Apache-2.0.
