# GridPin

[![PyPI](https://img.shields.io/pypi/v/gridpin)](https://pypi.org/project/gridpin/)
[![crates.io](https://img.shields.io/crates/v/gridpin)](https://crates.io/crates/gridpin)
[![DuckDB community extension](https://img.shields.io/badge/DuckDB-community%20extension-fff000)](https://duckdb.org/community_extensions/extensions/gridpin_ext)
[![MCP server](https://img.shields.io/badge/MCP-server-blue)](https://gridpin.dev/mcp-geocoding)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/gridpin/gridpin/blob/main/LICENSE)
[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.23610378.svg)](https://doi.org/10.5281/zenodo.23610378)
[![CI](https://github.com/gridpin/gridpin/actions/workflows/ci.yml/badge.svg)](https://github.com/gridpin/gridpin/actions/workflows/ci.yml)
[![Open in Colab](https://colab.research.google.com/assets/colab-badge.svg)](https://colab.research.google.com/github/gridpin/gridpin/blob/main/examples/colab_quickstart.ipynb)
[![Live demo](https://static.streamlit.io/badges/streamlit_badge_black_white.svg)](https://gridpin.streamlit.app)

Offline forward and reverse geocoding from a memory-mapped country file.
*Off the grid. On the grid.*

No server, no database import, no per-request fees, and every geocoding query
stays on your machine. After you install the package and download a sheet,
GridPin makes no network requests.

```bash
pip install --upgrade gridpin==0.5.0
```

The wheel contains the engine, not the data: **download a country sheet
separately**. Free static sheets for the United States, Germany, France, Italy, Spain, the Netherlands, Brazil and Serbia
are listed at <https://gridpin.dev/docs#delivery> — no account, no key, no
sign-up.

One command does the download and the check: `pip install gridpin-mcp`, then
`gridpin-fetch fr` puts the France sheet in `~/.cache/gridpin` and verifies it against the
signed release; from Python, `gridpin_mcp.sheets.download("fr")` returns the same path. The
same package is an MCP server for AI agents (`uvx gridpin-mcp`).

The United States is one nationwide sheet: point addresses in 35 states, Census address ranges
elsewhere. Germany covers **15 of 16 Länder; Bavaria is not covered**. Spain covers all
52 postal provinces; Brazil is one nationwide sheet. Client update required:
use 0.5.0, because older engines have no US rules.

```python
import gridpin

g = gridpin.Geocoder("france.bin")
g.geocode("8 Boulevard du Port, Amiens", 1)
g.reverse(49.8945, 2.2949, 1)

# optional POI layer (cascade built in: address index first,
# POI only on weak results, exact addresses never overridden)
g = gridpin.Geocoder("france.bin", poi="fr_poi.bin")
```

If GridPin is useful to you, [a star on GitHub](https://github.com/gridpin/gridpin) helps other people find it.

Three limits worth knowing before you build on this:

- **Reverse** returns the nearest *indexed* address — or a street-centroid
  approximation — together with `distance_m`. It is not a rooftop fix.
- **Speed:** exact lookups run at ~3,000/s, measured on the France sheet and a
  single arm64 laptop core. Typo-tolerant fuzzy matching is much slower (tens of
  ms/query) — use `geocode_many()` to batch.
- **POI is France-only today** and opt-in. Address lookup is the core product;
  the POI layer is younger and never overrides an exact address match.

- Country files & docs: <https://gridpin.dev/docs>
- Quality benchmark, including where other geocoders score higher:
  <https://gridpin.dev/bench>
- Source: <https://github.com/gridpin/gridpin>
- Issues: <https://github.com/gridpin/gridpin/issues>
- Engine license: Apache-2.0. Data files carry their own source licenses —
  see <https://github.com/gridpin/gridpin/blob/main/ATTRIBUTIONS.md>
