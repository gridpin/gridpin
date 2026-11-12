// GridPin in WebAssembly: load gridpin_wasm.wasm and a sheet, then geocode in the page (or in Node), no server.
//
//   import { GridPin } from "./gridpin.js";
//   const gp = await GridPin.load(wasmBytes, sheetBytes);   // ArrayBuffer or Uint8Array of each
//   gp.geocode("boulevard des moulins 27, monaco", 1);     // [{lat, lon, street, housenumber, ...}]
//   gp.reverse(43.74, 7.42, 1);

// Where the engine file lies next to this module (the npm package gridpin-wasm ships both):
//   fetch(wasmUrl) in a page, readFile(wasmUrl) in Node.
export const wasmUrl = new URL("./gridpin_wasm.wasm", import.meta.url);

export class GridPin {
  static async load(wasmBytes, sheetBytes) {
    const { instance } = await WebAssembly.instantiate(wasmBytes, {});
    const gp = new GridPin(instance.exports);
    gp.open(sheetBytes);
    return gp;
  }

  constructor(exports) {
    this.x = exports;
  }

  bytes() {
    return new Uint8Array(this.x.memory.buffer);
  }

  result() {
    const p = this.x.gw_result_ptr(), n = this.x.gw_result_len();
    return new TextDecoder().decode(this.bytes().slice(p, p + n));
  }

  open(sheetBytes) {
    const data = sheetBytes instanceof Uint8Array ? sheetBytes : new Uint8Array(sheetBytes);
    const p = this.x.gw_alloc(data.length);
    this.bytes().set(data, p);
    if (this.x.gw_open(p, data.length) !== 0) throw new Error(this.result());
  }

  geocode(query, k = 1) {
    const data = new TextEncoder().encode(query);
    const p = this.x.gw_alloc(data.length);
    this.bytes().set(data, p);
    const rc = this.x.gw_geocode(p, data.length, k);
    this.x.gw_free(p, data.length);
    if (rc !== 0) throw new Error(this.result());
    return JSON.parse(this.result());
  }

  reverse(lat, lon, k = 1) {
    if (this.x.gw_reverse(lat, lon, k) !== 0) throw new Error(this.result());
    return JSON.parse(this.result());
  }
}
