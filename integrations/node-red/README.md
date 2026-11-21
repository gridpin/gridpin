# node-red-contrib-gridpin

Offline geocoding in Node-RED. The `gridpin` node asks a GridPin server on your own machine (`gridpin serve`): no API
key, no cloud service, one file per country.

```bash
gridpin sheets fetch fr && gridpin serve france.bin     # listening on http://127.0.0.1:8080
```

| `msg.payload` in | `msg.payload` out |
|---|---|
| an address (string) | up to *Answers* hits: `lat`, `lon`, `precision`, `street`, `housenumber`, `commune`, `postcode`, `weak` |
| an array of addresses | one array of hits per address, in order, in one request |
| `{lat, lon}` | the nearest addresses of the point |

`msg.gridpin` counts the addresses asked, the weak answers (check them before use) and the empty ones. Addresses travel
only in the body of a POST, never in a URL. A stopped server or an error answer goes to the Catch node with a plain
message (`GridPin server not running at http://127.0.0.1:8080`).

## Install

Not in the Node-RED library yet. From the folder of your Node-RED (`~/.node-red`):

```bash
npm install /path/to/gridpin/code/integrations/node-red
```

## Tests

`test/gridpin.test.mjs` checks the logic of the node with plain `node --test`, no Node-RED needed; `make test-node-red` (from `code/`) also runs the node in a real Node-RED runtime (node-red-node-test-helper) against
`gridpin serve` on the Monaco smoke sheet. Checked with Node-RED 4.1.17 on Node 24.
