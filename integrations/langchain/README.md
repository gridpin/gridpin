# langchain-gridpin — offline geocoding tools for LangChain

LangChain tools that turn addresses into coordinates and coordinates into addresses with
[GridPin](https://gridpin.dev): one memory-mapped file per country ("sheet"), read on your
machine. No API key, no rate limit, and no query leaves the machine. The only network step is
downloading a sheet once, verified against the signed GridPin release.

Covered countries and sheet sizes: [gridpin.dev](https://gridpin.dev).

## Install

```console
$ pip install langchain-gridpin
$ gridpin-fetch us        # once: downloads and verifies the sheet, prints its path
```

`gridpin-fetch` comes with the `gridpin-mcp` dependency. Sheets go to `$GRIDPIN_HOME`, else
`$XDG_CACHE_HOME/gridpin`, else `~/.cache/gridpin`. From Python:

```python
from langchain_gridpin import download_sheet

download_sheet("us")   # no network when the sheet is already there
```

## Use the tools

```python
from langchain_gridpin import GridPinGeocode, GridPinReverse

geocode = GridPinGeocode(country="us")
geocode.invoke({"query": "1600 Pennsylvania Ave NW, Washington, DC 20500"})
# '{"country": "us", "query": "1600 Pennsylvania Ave NW, Washington, DC 20500",
#   "results": [{"lat": 38.8976751, "lon": -77.036547, "precision": "house", ...}]}'

reverse = GridPinReverse(country="us")
reverse.invoke({"lat": 38.8977, "lon": -77.0365})
# '{"country": "us", "lat": 38.8977, "lon": -77.0365,
#   "results": [{"housenumber": "1600", "street": "PENNSYLVANIA Avenue Northwest", "distance_m": 4.9, ...}]}'
```

Both tools for one country, for an agent (`pip install langchain langchain-anthropic`, or any
other chat model with tool calling):

```python
from langchain.agents import create_agent
from langchain_gridpin import GridPinToolkit

agent = create_agent("anthropic:claude-sonnet-5-5", tools=GridPinToolkit(country="us").get_tools())
agent.invoke({"messages": [{"role": "user", "content": "Where is 350 5th Ave, New York?"}]})
```

| Tool | Arguments | Returns |
|---|---|---|
| `GridPinGeocode` (name `gridpin_geocode_<country>`) | `query`, `k` (1-10, default 1) | ranked candidates with `lat`, `lon`, `precision`, `confidence`, `street`, `housenumber`, `commune`, `postcode` |
| `GridPinReverse` (name `gridpin_reverse_<country>`) | `lat`, `lon`, `k` (1-10, default 1) | nearest indexed addresses, each with `distance_m` |

Constructor parameters of both tools and of `GridPinToolkit`:

- `country` — country code or English name of the sheet, e.g. `"us"`. An unknown country fails
  at construction and the error lists the available ones.
- `sheet` — optional path to a sheet file, instead of the downloaded one.

The tools use `response_format="content_and_artifact"`: called by an agent, the `ToolMessage`
carries the answer as JSON text in `content` and as a dict in `artifact`. Called directly with a
dict, as above, they return the JSON text. Tools reading the same sheet share one open file.

A missing sheet or an empty query comes back to the model as an error message it can act on
(for example "run `gridpin-fetch us`"); set `handle_tool_error=False` to get an exception
instead. An address that is not found is not an error: `results` is an empty list.

Results are the same as the GridPin CLI, Python, DuckDB and MCP server return. Treat `street`
or `city` precision as approximate.

## Links

- GridPin: [gridpin.dev](https://gridpin.dev), [documentation](https://gridpin.dev/docs),
  [benchmark](https://gridpin.dev/bench)
- Source: [github.com/gridpin/gridpin](https://github.com/gridpin/gridpin) (this package lives in
  `integrations/langchain/`)
- MCP server for other agent frameworks: [gridpin-mcp](https://pypi.org/project/gridpin-mcp/)

If GridPin is useful to you, [a star on GitHub](https://github.com/gridpin/gridpin) helps other
people find it.

License: Apache-2.0.
