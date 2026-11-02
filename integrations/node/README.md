# gridpin for Node.js

Offline geocoder: an address to coordinates and back, from one local file per country (a "sheet"). No API
key, no rate limit, no query leaves the machine. The same engine as the `gridpin` CLI, Python package, R
package and DuckDB extension, through its C interface.

```js
const { Geocoder, sheets } = require('gridpin');

const sheet = await sheets.fetch('us');          // once: download, check size and SHA-256 against the signed release
const g = new Geocoder(sheet);
g.geocode('1600 Pennsylvania Ave NW, Washington, DC', { k: 1 });
// [{ lat: 38.8976751, lon: -77.036547, precision: 'house', street: 'PENNSYLVANIA Avenue Northwest',
//    housenumber: '1600', commune: 'Washington', postcode: '20500', confidence: 0.95, weak: false, ... }]
g.geocodeMany(['10 Downing St', null, '350 5th Ave, New York']);   // one list per row, input order kept
g.reverse(38.8977, -77.0365);
g.close();
```

- `weak: true` marks an answer the engine itself says needs a check before use (city level, low confidence,
  or a fuzzy street with middling confidence). An exact house on an exact street is never weak.
- Sheets live where the CLI and gridpin-mcp keep them: `GRIDPIN_HOME`, else `$XDG_CACHE_HOME/gridpin`, else
  `~/.cache/gridpin`. `sheets.list()`, `sheets.where('us')`, `sheets.verify('us')`; `GRIDPIN_OFFLINE=1` forbids
  downloads.
- `geocodeMany` runs on the calling thread: for large batches, use a worker thread.

Countries, data sources and licenses: https://gridpin.dev. Engine: Apache-2.0. Sheet data: OpenStreetMap
contributors (ODbL), Overture Maps and national open address sources.
