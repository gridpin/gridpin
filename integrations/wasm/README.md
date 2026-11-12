# GridPin in WebAssembly

Geocode in a browser page or in Node from a country sheet held in memory: no server, no network after the sheet is
loaded. The engine is the same as the CLI's; answers are equal field for field (see `test/`).

From npm (the package carries the built engine):

```sh
npm install gridpin-wasm
```

```js
import { GridPin, wasmUrl } from "gridpin-wasm";
const gp = await GridPin.load(await (await fetch(wasmUrl)).arrayBuffer(),
                              await (await fetch("mc.bin")).arrayBuffer());
gp.geocode("boulevard des moulins 27, monaco", 1);   // [{lat, lon, street, housenumber, match_level, ...}]
gp.reverse(43.74, 7.42, 1);
```

In Node: `GridPin.load(await readFile(wasmUrl), await readFile("mc.bin"))`. To build the engine yourself:

```sh
rustup target add wasm32-unknown-unknown   # once
./build.sh                                 # -> gridpin_wasm.wasm (about 1.5 MB)
```

`demo.html` does the same with a file picker. The whole sheet is read into the page's memory (Monaco: 130 KB; a
large country: hundreds of MB, so a small country or a cut of a sheet (`gridpin slice`) suits a browser best).
No wasm-bindgen: the module exports `gw_alloc`, `gw_free`, `gw_open`, `gw_geocode`, `gw_reverse`, `gw_result_ptr`,
`gw_result_len`. Licence: Apache-2.0; `THIRDPARTY.yml` holds the licences of the Rust libraries compiled into the engine; the sheet's
data keeps its own licence (`gridpin meta`).
