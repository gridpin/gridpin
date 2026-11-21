// GridPin in WebAssembly: load gridpin_wasm.wasm and a sheet, then geocode in the page (or in Node), no server.
//
//   import { GridPin } from "./gridpin.js";
//   const gp = await GridPin.load(wasmBytes, sheetBytes);   // ArrayBuffer or Uint8Array of each
//   gp.geocode("boulevard des moulins 27, monaco", 1);     // [{lat, lon, street, housenumber, ...}]
//   gp.reverse(43.74, 7.42, 1);
//   gp.suggest("boulevard des mou", 5);                   // [{label, street, commune, lat, lon, precision}]

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

  // The suggestions of an address field for what is typed so far, as the server's /v1/suggest: [{label, street,
  // commune, lat, lon, precision}]. `near` = [lat, lon] puts the streets near that point first.
  suggest(query, k = 8, near = null) {
    const data = new TextEncoder().encode(query);
    const p = this.x.gw_alloc(data.length);
    this.bytes().set(data, p);
    const [lat, lon] = near ?? [0, 0];
    const rc = this.x.gw_suggest(p, data.length, k, near ? 1 : 0, lat, lon);
    this.x.gw_free(p, data.length);
    if (rc !== 0) throw new Error(this.result());
    return JSON.parse(this.result());
  }

  reverse(lat, lon, k = 1) {
    if (this.x.gw_reverse(lat, lon, k) !== 0) throw new Error(this.result());
    return JSON.parse(this.result());
  }
}

// WebMCP: let the browser's agent call geocode and reverse of the loaded sheet (`document.modelContext.registerTool`,
// draft of the W3C Web Machine Learning group, read 2026-11-14). Returns false where the browser has no such API, so a
// page stays as it is; nothing leaves the page, the tools answer from the sheet in memory. Abort `signal` to remove them.
const MAX_QUERY = 1000;

export async function registerModelContextTools(gp, { modelContext = globalThis.document?.modelContext, signal } = {}) {
  if (!(gp instanceof GridPin)) throw new Error("load a sheet first: the tools answer from it");
  if (!modelContext || typeof modelContext.registerTool !== "function") return false;
  const ok = (results) => ({ content: [{ type: "text", text: JSON.stringify(results) }], structuredContent: { results } });
  const fail = (message) => ({ content: [{ type: "text", text: message }], isError: true });
  const limitOf = (v) => (Number.isInteger(v) && v >= 1 && v <= 10 ? v : 1);
  await modelContext.registerTool({
    name: "gridpin_geocode",
    description: "Find the coordinates of an address in the country sheet loaded in this page. Each answer has " +
      "precision (house, interp, street, city) and weak: true when the answer needs a check before use.",
    inputSchema: {
      type: "object",
      properties: {
        query: { type: "string", description: "The address as a person writes it", maxLength: MAX_QUERY },
        limit: { type: "integer", minimum: 1, maximum: 10, description: "How many answers, 1 by default" },
      },
      required: ["query"],
    },
    execute({ query, limit } = {}) {
      const q = typeof query === "string" ? query.trim() : "";
      if (!q) return fail("query is empty: give an address");
      if (q.length > MAX_QUERY) return fail(`query is over ${MAX_QUERY} characters`);
      try {
        return ok(gp.geocode(q, limitOf(limit)));
      } catch (e) {
        return fail(String(e.message || e));
      }
    },
  }, { signal });
  await modelContext.registerTool({
    name: "gridpin_reverse",
    description: "Find the nearest addresses to a point in the country sheet loaded in this page; each answer has " +
      "distance_m from the point.",
    inputSchema: {
      type: "object",
      properties: {
        lat: { type: "number", minimum: -90, maximum: 90 },
        lon: { type: "number", minimum: -180, maximum: 180 },
        limit: { type: "integer", minimum: 1, maximum: 10 },
      },
      required: ["lat", "lon"],
    },
    execute({ lat, lon, limit } = {}) {
      if (!Number.isFinite(lat) || !Number.isFinite(lon) || Math.abs(lat) > 90 || Math.abs(lon) > 180) {
        return fail("lat must be within ±90 and lon within ±180");
      }
      try {
        return ok(gp.reverse(lat, lon, limitOf(limit)));
      } catch (e) {
        return fail(String(e.message || e));
      }
    },
  }, { signal });
  return true;
}
