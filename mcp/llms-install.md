# Installing the GridPin MCP server (instructions for AI agents)

GridPin geocodes addresses offline from one data file per country ("sheet"). The server runs on the
user's machine over stdio. It needs no API key and no account. The only network step is downloading
a country sheet once.

## 1. Requirements

- Python 3.10 or newer, and `uv` (preferred) or `pip`.
- A platform with a prebuilt `gridpin` wheel: macOS on Apple Silicon, Linux x86_64 (glibc 2.34+),
  Windows x86_64. On other platforms `pip` builds the engine from source and needs a Rust toolchain.
- Free disk space for the sheets the user needs: Serbia 32 MB, Netherlands 103 MB, Spain 185 MB,
  Germany 213 MB (no Bavaria), Italy 247 MB, France 365 MB, Brazil 677 MB, Canada 183 MB, United States 1452 MB (1.45 GB).

## 2. Add the server to the MCP settings

With `uv` installed, add this entry to the MCP settings file (for Cline: `cline_mcp_settings.json`):

```json
{
  "mcpServers": {
    "gridpin": {
      "command": "uvx",
      "args": ["gridpin-mcp"],
      "disabled": false,
      "autoApprove": []
    }
  }
}
```

Without `uv`: run `pip install gridpin-mcp`, then use `"command": "gridpin-mcp"` and `"args": []`.

Optional: set `"env": {"GRIDPIN_HOME": "/path/to/dir"}` to choose where sheets are stored
(default `~/.cache/gridpin`). Set it in this server entry: some MCP clients start the server with
only a few environment variables, so a `GRIDPIN_HOME` exported in the user's shell does not reach it.

## 3. Verify

1. Call `list_countries`. It returns nine countries (`us`, `ca`, `de`, `fr`, `it`, `es`, `nl`, `br`, `rs`), each
   with its size and whether it is downloaded.
2. Ask the user which countries they need, then call `download_sheet` once per country. The download is
   checked against the signed release; a mismatch is discarded. A large sheet can take longer than the
   client allows for one tool call; if the call times out, ask the user to run
   `uvx --from gridpin-mcp gridpin-fetch <code>` (or `gridpin-fetch <code>` after `pip install gridpin-mcp`)
   in a terminal with the same `GRIDPIN_HOME` as the server, then call `list_countries` again.
3. Call `geocode` with an address in that country, e.g. `{"query": "Knez Mihailova 5, Beograd",
   "country": "rs"}`. A good result has `"precision": "house"`.

## Tools

| Tool | Purpose |
|---|---|
| `list_countries` | covered countries, sheet sizes, download state |
| `download_sheet` | fetch and verify one country sheet; the only tool that uses the network |
| `geocode` | address -> ranked candidates (`lat`, `lon`, `precision`, `confidence`, ...) |
| `reverse` | coordinates -> nearest indexed addresses with `distance_m` |
| `batch` | up to 1000 addresses of one country per call, in input order |

Errors name the next step, for example "the France sheet is not downloaded (365 MB); run
`gridpin-fetch fr` or call the download_sheet tool".
