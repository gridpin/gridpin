# GridPin

Offline forward and reverse geocoding from a memory-mapped country file.
*Off the grid. On the grid.*

No server, no database import, no per-request fees, and every geocoding query
stays on your machine. After you install the package and download a sheet,
GridPin makes no network requests.

```bash
pip install gridpin
```

The wheel contains the engine, not the data: **download a country sheet
separately**. Free static sheets for France, Italy, the Netherlands and Serbia
are listed at <https://gridpin.dev/docs.html#delivery> — no account, no key, no
sign-up.

```python
import gridpin

g = gridpin.Geocoder("france.bin")
g.geocode("8 Boulevard du Port, Amiens", 1)
g.reverse(49.8945, 2.2949, 1)

# optional POI layer (cascade built in: address index first,
# POI only on weak results, exact addresses never overridden)
g = gridpin.Geocoder("france.bin", poi="fr_poi.bin")
```

Three limits worth knowing before you build on this:

- **Reverse** returns the nearest *indexed* address — or a street-centroid
  approximation — together with `distance_m`. It is not a rooftop fix.
- **Speed:** exact lookups run at ~3,000/s, measured on the France sheet and a
  single arm64 laptop core. Typo-tolerant fuzzy matching is much slower (tens of
  ms/query) — use `geocode_many()` to batch.
- **POI is France-only today** and opt-in. Address lookup is the core product;
  the POI layer is younger and never overrides an exact address match.

- Country files & docs: <https://gridpin.dev/docs.html>
- Quality benchmark, including where other geocoders score higher:
  <https://gridpin.dev/bench.html>
- Source: <https://github.com/gridpin/gridpin>
- Issues: <https://github.com/gridpin/gridpin/issues>
- Engine license: Apache-2.0. Data files carry their own source licenses —
  see <https://github.com/gridpin/gridpin/blob/main/ATTRIBUTIONS.md>
