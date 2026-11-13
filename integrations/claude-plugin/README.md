# GridPin Offline Geocoder for Claude

Geocode addresses to coordinates and back on your own machine. GridPin reads one downloadable file per country, so there is no API key and no rate limit, and your addresses are not sent to a geocoding service.

The plugin adds six tools (`list_countries`, `download_sheet`, `geocode`, `reverse`, `batch`, `extract_addresses`) and a skill that tells Claude when to use them and how to read the answer, including when an answer is only approximate.

Try it after installing:

> Geocode the addresses in `customers.csv` and add latitude and longitude columns. Tell me which rows are only street-level.

## Requirements

- [`uv`](https://docs.astral.sh/uv/), because the plugin starts the server with `uvx`.
- Claude Code, or Cowork running on your computer. The server is local, so Claude in the browser does not start it.
- Disk space for the country files you use, from 32 MB to about 1.5 GB each. `list_countries` shows the sizes.

## What it runs, downloads and sends

- **Runs:** `uvx gridpin-mcp==0.6.0`, the open-source [GridPin MCP server](https://github.com/gridpin/gridpin/tree/main/mcp). On first start, `uv` downloads that package and its dependencies from PyPI.
- **Downloads:** when `download_sheet` is called, one country file from `dl.gridpin.dev`, checked against the SHA-256 in the signed release. Files are kept in `~/.cache/gridpin` unless you set `GRIDPIN_HOME` in the server's environment.
- **Sends:** the plugin has no telemetry and no account, and its tools send addresses and coordinates nowhere. As with any download, the file host and PyPI see the request itself, not your addresses.

Anything you put in the conversation, addresses included, still reaches Claude, under the terms of your Claude plan. Privacy policy: https://gridpin.dev/privacy

## Quality

GridPin is not the most accurate geocoder on every kind of input. Measured results against Nominatim and Photon, including the sets where they win, are at https://gridpin.dev/bench.

## Data and licence

The plugin and the server are Apache-2.0. The country files are built from open data under their own terms; see [the attributions](https://github.com/gridpin/gridpin/blob/main/ATTRIBUTIONS.md).

GridPin is built and maintained with the help of AI coding agents.

Questions and bugs: https://github.com/gridpin/gridpin/issues
