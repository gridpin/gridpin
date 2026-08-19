-- Geocode a table of order addresses with the gridpin DuckDB extension.
-- Run: duckdb -unsigned < orders.sql   (adjust the country file path below first)

-- Once gridpin_ext is in the DuckDB community catalog: INSTALL gridpin_ext FROM community;
-- Until then: download gridpin_ext-<platform>.duckdb_extension from GitHub releases,
-- start `duckdb -unsigned`, and load the file by path:
LOAD 'gridpin_ext-osx_arm64.duckdb_extension';

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
