# gridpin-mcp — offline geocoding for AI agents

<!-- mcp-name: io.github.gridpin/gridpin-mcp -->

An [MCP](https://modelcontextprotocol.io) server that lets an agent geocode addresses with
[GridPin](https://gridpin.dev): one memory-mapped file per country, read on your machine.
No API key, no rate limit, and no query leaves the machine. The only network step is
downloading a country sheet once, verified against the signed GridPin release.

Countries (release v0.4.0): Brazil, France, Germany, Italy, Netherlands, Serbia, Spain.

## Add it to an agent

Claude Code:

```console
$ claude mcp add gridpin -- uvx gridpin-mcp
```

Any client that reads an `mcpServers` config (Claude Desktop, Cursor, ...):

```json
{ "mcpServers": { "gridpin": { "command": "uvx", "args": ["gridpin-mcp"] } } }
```

Step-by-step setup for agents (requirements, settings, verification): [llms-install.md](llms-install.md).

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
$ gridpin-fetch fr          # prints the path, e.g. ~/.cache/gridpin/v0.4.0/france.bin
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
