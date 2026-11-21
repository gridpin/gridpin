# GridPin in KNIME and FME

Offline geocoding of an address column with the country file on your own machine. **Both recipes follow the vendors'
documentation (read 16.11.2026) and have not been run in KNIME or FME**: the step they call — `gridpin.geocode_df` and
`POST /v1/geocode` — is the one checked everywhere else in GridPin.

## KNIME Analytics Platform: a Python Script node

1. In the Python environment the node uses (Preferences > KNIME > Python), add the package: `pip install "gridpin[pandas]"`,
   and download the country file once: `python -c 'import gridpin; print(gridpin.sheets.fetch("fr"))'`.
2. Add a **Python Script** node after the table with an `Address` column and paste:

```python
import knime.scripting.io as knio
import gridpin

df = knio.input_tables[0].to_pandas()
out = gridpin.geocode_df(df, "Address", "fr")        # adds gp_lat, gp_lon, gp_precision, gp_weak, …
knio.output_tables[0] = knio.Table.from_pandas(out)
```

Every row stays; a row without an address or an answer gets empty `gp_*` cells and `gp_match_status` `U`. `gp_weak`
true: check that answer before use. `knime.scripting.io` is the scripting module of KNIME 4.7 and later.

## FME: HTTPCaller to the local server

Start `gridpin serve france.bin` on the machine that runs the workspace. In the workspace:

1. **JSONTemplater** (or an AttributeCreator) builds the body `{"queries": ["<address>"], "k": 1}`.
2. **HTTPCaller**: method POST, URL `http://127.0.0.1:8080/v1/geocode`, header `Content-Type: application/json`, body
   the attribute of step 1; response into `_response_body`.
3. **JSONFragmenter** / **JSONFlattener** on `json["results"][0][0]` gives `lat`, `lon`, `precision`, `weak`.
4. **VertexCreator** from `lat`, `lon` makes the point (EPSG:4326).

The address goes in the body of the request, never in the URL. One request per feature is the simple form; for large
tables, aggregate addresses into one list per request (up to a few thousand) and explode the answers after.
