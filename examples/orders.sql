-- Geocode a table of order addresses with the gridpin DuckDB extension.
-- Run: duckdb < orders.sql   (adjust the country file path below first)

-- From DuckDB's community catalog. It serves the current release on DuckDB 1.5.6 or newer;
-- older DuckDB versions install an older build. Check what you got with
-- SELECT extension_version FROM duckdb_extensions() WHERE extension_name = 'gridpin_ext';
INSTALL gridpin_ext FROM community;
LOAD gridpin_ext;

-- Point the extension at a country build file.
SELECT gridpin_load('/path/to/france.bin');

CREATE OR REPLACE TABLE orders (id INTEGER, address VARCHAR);
INSERT INTO orders VALUES
    (1, '10 rue de Rivoli, 75004 Paris'),
    (2, '1 place Bellecour, 69002 Lyon'),
    (3, '35 boulevard Michelet, 13008 Marseille');

WITH geocoded AS (
    SELECT id, address, gridpin_geocode(address) AS g
    FROM orders
)
SELECT
    id,
    address,
    json_extract(g, '$.lat')        AS lat,
    json_extract(g, '$.lon')        AS lon,
    json_extract(g, '$.confidence') AS confidence
FROM geocoded;
