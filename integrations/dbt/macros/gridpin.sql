{#- GridPin in dbt: geocode an address column inside the warehouse.
    dbt-duckdb, DuckDB 1.5.6+ (older DuckDB versions install an older build): the community extension `gridpin_ext` (INSTALL gridpin_ext FROM community; LOAD gridpin_ext;
    SELECT gridpin_load('fr.bin') in an on-run-start hook) gives gridpin_geocode(text) -> JSON text.
    Postgres: integrations/postgres/gridpin.sql gives gridpin_geocode(text) -> jsonb.

    gridpin_lat / gridpin_lon / gridpin_level each geocode the address again. For more than one field, geocode once
    into a column (gridpin_geocode) and read the fields of that answer with gridpin_field: see
    models/example_geocoded.sql. A weak answer (field `weak` = 'true') is one the engine says needs checking. -#}

{% macro gridpin_geocode(column) -%}
  {{ return(adapter.dispatch('gridpin_geocode', 'gridpin')(column)) }}
{%- endmacro %}

{% macro duckdb__gridpin_geocode(column) -%}
  gridpin_geocode({{ column }})
{%- endmacro %}

{% macro postgres__gridpin_geocode(column) -%}
  gridpin_geocode({{ column }})::text
{%- endmacro %}

{% macro default__gridpin_geocode(column) -%}
  gridpin_geocode({{ column }})
{%- endmacro %}

{#- One field of an answer (the JSON text gridpin_geocode gives) as text; NULL when there is no answer. -#}
{% macro gridpin_field(answer, field) -%}
  {{ return(adapter.dispatch('gridpin_field', 'gridpin')(answer, field)) }}
{%- endmacro %}

{% macro duckdb__gridpin_field(answer, field) -%}
  json_extract_string({{ answer }}, '$.{{ field }}')
{%- endmacro %}

{% macro postgres__gridpin_field(answer, field) -%}
  (({{ answer }})::jsonb ->> '{{ field }}')
{%- endmacro %}

{% macro default__gridpin_field(answer, field) -%}
  json_extract_string({{ answer }}, '$.{{ field }}')
{%- endmacro %}

{% macro gridpin_lat(column) -%}
  cast({{ gridpin.gridpin_field(gridpin.gridpin_geocode(column), 'lat') }} as double precision)
{%- endmacro %}

{% macro gridpin_lon(column) -%}
  cast({{ gridpin.gridpin_field(gridpin.gridpin_geocode(column), 'lon') }} as double precision)
{%- endmacro %}

{% macro gridpin_level(column) -%}
  {{ gridpin.gridpin_field(gridpin.gridpin_geocode(column), 'match_level') }}
{%- endmacro %}
