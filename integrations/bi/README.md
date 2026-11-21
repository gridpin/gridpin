# GridPin in Tableau and Power BI

Offline geocoding of an address column, with the country file on your own machine: no API key, no cloud geocoder.

## Tableau, through TabPy

TabPy (Tableau's analytics extension server) runs the four functions of `tabpy/gridpin_tabpy.py`. On the machine
of the TabPy server:

```bash
pip install tabpy gridpin
sheet=$(python -c 'import gridpin; print(gridpin.sheets.fetch("fr"))')   # the country file, once
tabpy --disable-auth-warning &                                              # http://localhost:9004, this machine only
python tabpy/gridpin_tabpy.py --sheet "$sheet"
```

In Tableau: Help > Settings and Performance > Manage Analytics Extension Connection > TabPy, `localhost`, `9004`.
Then calculated fields that ask a whole column at once:

```text
SCRIPT_REAL("return tabpy.query('gridpin_lat', _arg1)['response']", ATTR([Address]))
SCRIPT_REAL("return tabpy.query('gridpin_lon', _arg1)['response']", ATTR([Address]))
SCRIPT_STR("return tabpy.query('gridpin_label', _arg1)['response']", ATTR([Address]))
SCRIPT_BOOL("return tabpy.query('gridpin_weak', _arg1)['response']", ATTR([Address]))
```

`gridpin_weak` is true for an answer to check before use (town level, low confidence). An empty address and an address
without an answer give a null.

## Power BI, through Power Query

Start the local server (`gridpin serve france.bin`), then in Power BI Desktop: Get data > Blank query > Advanced
editor, paste `powerbi/GridPin.pq`, name the query `GridPin.Geocode`, and add a step to your table:

```text
= GridPin.Geocode(Source, "Address")
```

It adds `gp_lat`, `gp_lon`, `gp_label` and `gp_weak`, asking the server 1,000 addresses per request; addresses travel
in the body of the request, never in a URL. Power BI Service does not see `127.0.0.1`: a scheduled refresh in the
service needs an on-premises data gateway on the machine that runs the server.

## Where this is checked

| Part | Checked |
|---|---|
| TabPy functions | run on TabPy 2.16.0: 102 rows of French addresses through `/evaluate`, as Tableau calls it, equal to the engine's own answers |
| Tableau Desktop | not run: a commercial program |
| Power Query function | not run: Power BI Desktop is Windows only — waiting for a check on Windows |
