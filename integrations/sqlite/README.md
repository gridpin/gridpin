# GridPin for SQLite

A loadable extension: geocode the addresses of a table inside SQLite (and Datasette), offline.

```sh
./build.sh            # builds gridpin-ffi and gridpin_sqlite.dylib / .so
```

```sql
.load ./gridpin_sqlite
SELECT gridpin_open('fr.bin');                                -- the country sheet, once per connection
SELECT gridpin_geocode('10 rue de Rivoli, Paris');            -- the best answer as JSON, '{}' when none
SELECT gridpin_lat('10 rue de Rivoli, Paris');                -- one coordinate, NULL when none

-- a table: geocode each address once, then read the fields of its answer
WITH g AS MATERIALIZED (SELECT id, gridpin_geocode(address) AS a FROM customers)
SELECT id, json_extract(a, '$.lat') AS lat, json_extract(a, '$.lon') AS lon,
       json_extract(a, '$.match_level') AS level, json_extract(a, '$.weak') AS weak FROM g;
```

- Each of `gridpin_geocode`, `gridpin_lat` and `gridpin_lon` geocodes its argument, so
  `SELECT gridpin_lat(address), gridpin_lon(address), …` geocodes every row two or three times. Without
  `MATERIALIZED`, SQLite copies the CTE into every field that reads it, and the address is geocoded once per field.
- `weak` = 1 marks an answer the engine itself says needs checking (a town for a full address, a far namesake): keep
  such rows out with `WHERE json_extract(a, '$.weak') = 0`, or check them.
- One sheet per connection. To open another: `SELECT gridpin_reset();` then `gridpin_open('other.bin')`, or open a
  new connection.
- A call before `gridpin_open` stops with "call gridpin_open('<path>.bin') first".

Python: `conn.enable_load_extension(True); conn.load_extension("./gridpin_sqlite")`.
Datasette: `datasette data.db --load-extension ./gridpin_sqlite` and call the functions in SQL.
A table function of several candidates (`gridpin_candidates(text, k)`) is not in this version.
Licence: Apache-2.0; the sheet's data keeps its own licence (`gridpin meta`).
