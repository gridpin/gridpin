# gridpin-mcp — offline geocoding for AI agents

<!-- mcp-name: io.github.gridpin/gridpin-mcp -->

An [MCP](https://modelcontextprotocol.io) server that lets an agent geocode addresses with
[GridPin](https://gridpin.dev): one memory-mapped file per country, read on your machine.
No API key, no rate limit, and no query leaves the machine. The only network step is
downloading a country sheet once, verified against the signed GridPin release.

Countries (release v0.5.0): United States, Germany, France, Italy, Spain, Netherlands, Brazil, Serbia.

## Add it to an agent

Claude Code:

```console
$ claude mcp add gridpin -- uvx gridpin-mcp
```

Any client that reads an `mcpServers` config (Claude Desktop, Cursor, ...):

```json
{ "mcpServers": { "gridpin": { "command": "uvx", "args": ["gridpin-mcp"] } } }
```

To keep sheets in another directory, pass `GRIDPIN_HOME` in the server's own environment:

```json
{ "mcpServers": { "gridpin": { "command": "uvx", "args": ["gridpin-mcp"],
                               "env": { "GRIDPIN_HOME": "/path/to/sheets" } } } }
```

Exporting it in your shell is not enough. Some MCP clients start the server with only a few
environment variables (clients built on the MCP Python SDK pass `HOME`, `LOGNAME`, `PATH`,
`SHELL`, `TERM` and `USER` on macOS and Linux), so `GRIDPIN_HOME` and `XDG_CACHE_HOME` from
your shell never reach it and sheets go to `~/.cache/gridpin`.

`download_sheet` fetches a whole sheet in one tool call, and the larger sheets can take longer
than a client allows for one call (mcp-haystack's `MCPToolset`, for example, stops after 30 s
by default). Raise the client's tool timeout, or download the sheet beforehand in a terminal,
with the same `GRIDPIN_HOME` as the server; the tool then returns at once:

```console
$ uvx --from gridpin-mcp gridpin-fetch fr
```

Step-by-step setup for agents (requirements, settings, verification): [llms-install.md](llms-install.md).

If GridPin is useful to you, [a star on GitHub](https://github.com/gridpin/gridpin) helps other people find it.

## Tools

| Tool | What it does |
|---|---|
| `list_countries` | covered countries, sheet sizes, which are downloaded |
| `download_sheet` | fetch one country sheet (32-677 MB) once, verify its SHA-256; the only tool that uses the network |
| `geocode` | address text -> ranked candidates with `lat`, `lon`, `precision`, `confidence` |
| `reverse` | coordinates -> nearest indexed addresses with `distance_m` |
| `batch` | up to 1000 addresses of one country per call, in input order |

Results are the same as the GridPin CLI, Python and DuckDB return. Treat `street` or
`city` precision as approximate.

## Download a sheet without MCP

```console
$ pip install gridpin-mcp
$ gridpin-fetch fr          # prints the path, e.g. ~/.cache/gridpin/v0.5.0/france.bin
```

```python
import gridpin
from gridpin_mcp import sheets

path = sheets.download("fr")   # no-op when already downloaded
gridpin.Geocoder(str(path)).geocode("1 rue de la Paix, Paris", 1)
```

Sheets go to `$GRIDPIN_HOME`, else `$XDG_CACHE_HOME/gridpin`, else `~/.cache/gridpin`.
Sizes and SHA-256 of every sheet are pinned in the package from the signed release
attestation; a download that does not match is discarded.

## License

Apache-2.0. Sheet data: see [attributions](https://github.com/gridpin/gridpin/blob/main/ATTRIBUTIONS.md).
