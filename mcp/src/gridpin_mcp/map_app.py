"""A map in the chat for clients of MCP Apps (`io.modelcontextprotocol/ui`, edition 2026-01-26): `show_on_map`.

The tool geocodes a list of addresses of one country and returns the pins. A client that draws MCP Apps (Claude on the
web and Desktop, VS Code, ChatGPT, Goose) shows them on `ui://gridpin/map.html`, a page with no outside host. The base
map is the density of the sheet's own addresses: a coarse grid over the whole frame, where
built-up land reads as shade, and a fine 25 m grid around each pin, where blocks and streets show. Both grids are byte
arrays compressed with zlib and sent as base64; the page unpacks them with `DecompressionStream`, so it needs no
tile server and no image URL. A weak answer is a hollow pin, named "weak" on click. A client without MCP Apps gets the
pins only, without the grids.

`extension(session)` returns the `Apps` instance; the server adds it on the day of a release
(`MCPServer(extensions=[...])`), since the list of tools is compared by checks outside this repository.
"""

from __future__ import annotations

import base64
import math
import zlib
from collections import Counter
from typing import Any, Dict, List, Tuple

from typing_extensions import NotRequired, TypedDict

from mcp.server.apps import Apps, ResourceCsp, client_supports_apps
from mcp.server.mcpserver.context import Context
from mcp.types import ToolAnnotations

from . import tools

URI = "ui://gridpin/map.html"
MAX_PINS = 200
# the frame's addresses are read at most FRAME_POINTS at a time: one read costs about 2.5 s whatever the limit, and
# a city fits (Belgrade: 376 000 houses); a frame as wide as a country is sampled evenly
FRAME_POINTS = 1_000_000
# the coarse grid: the long side of the frame in at most OVERVIEW_CELLS cells, a cell never under FINE_M
OVERVIEW_CELLS = 256
# the fine grid: FINE_M cells in a square of NEAR_M metres each way from a pin, on one lattice for the whole frame so
# that two windows that overlap show the same cells
FINE_M = 25
NEAR_M = 250
# when the frame was sampled, the fine grid of a pin needs its own read of the sheet (about 0.2 s): the first
# NEAR_READS pins get one, the rest only the coarse grid
NEAR_READS = 30
# a fine cell with FINE_FULL addresses or more is drawn at full shade
FINE_FULL = 4
M_PER_DEG = 111_320.0


class Pin(TypedDict):
    query: str
    label: str
    lat: float
    lon: float
    precision: str
    weak: bool


class Grid(TypedDict):
    """Cells of address density, west to east in rows from south to north: byte 0 is empty, 255 the densest.

    `cells` is base64 of zlib of size[0] * size[1] bytes per window; `origins` holds the south-west corner (lon, lat) of
    each window, in degrees, and `step` the cell size (lon, lat) in degrees."""

    step: List[float]
    size: List[int]
    origins: List[List[float]]
    cells: str


class Base(TypedDict):
    overview: Grid
    near: Grid
    complete: bool


class MapOut(TypedDict):
    country: str
    pins: List[Pin]
    missing: List[str]
    note: NotRequired[str]
    base: NotRequired[Base]


def _label(hit: Dict[str, Any]) -> str:
    number = hit.get("housenumber") or ""
    street = hit.get("street") or ""
    place = " ".join(p for p in (hit.get("postcode") or "", hit.get("commune") or "") if p)
    head = f"{number} {street}".strip()
    return ", ".join(p for p in (head, place) if p) or "answer"


def map_view(session: "tools.Session", queries: List[str], country: str, with_base: bool = True) -> MapOut:
    """The pins of `queries` (the top answer of each) and, for a client that draws, the base points around them."""
    if not isinstance(queries, list) or not queries:
        raise tools.ToolError("queries must be a non-empty list of address strings")
    if not all(isinstance(q, str) and q.strip() for q in queries):
        raise tools.ToolError("every query must be a non-empty string")
    code, geo = session.geocoder(country)
    shown = queries[:MAX_PINS]
    rows = geo.geocode_many(shown, 1)
    pins: List[Pin] = []
    missing: List[str] = []
    for q, answers in zip(shown, rows):
        answers = list(answers)
        if not answers:
            missing.append(q)
            continue
        h = answers[0]
        pins.append({
            "query": q,
            "label": _label(h),
            "lat": h["lat"],
            "lon": h["lon"],
            "precision": h.get("precision", ""),
            "weak": bool(h.get("weak")),
        })
    out: MapOut = {"country": code, "pins": pins, "missing": missing}
    if len(queries) > MAX_PINS:
        out["note"] = f"shown the first {MAX_PINS} of {len(queries)} addresses"
    if with_base and pins:
        out["base"] = _base(geo, pins)
    return out


def _fine_shade(count: int) -> int:
    return 0 if count <= 0 else 150 + 105 * min(count, FINE_FULL) // FINE_FULL


def _pack(data: bytes) -> str:
    return base64.b64encode(zlib.compress(data, 9)).decode("ascii")


def _base(geo: Any, pins: List[Pin]) -> Base:
    """The two density grids around `pins`, read from the sheet."""
    lats = [p["lat"] for p in pins]
    lons = [p["lon"] for p in pins]
    pad_lat = max(0.005, (max(lats) - min(lats)) * 0.2)
    pad_lon = max(0.007, (max(lons) - min(lons)) * 0.2)
    box = (min(lons) - pad_lon, min(lats) - pad_lat, max(lons) + pad_lon, max(lats) + pad_lat)
    k = math.cos(math.radians((box[1] + box[3]) / 2))
    # one fine lattice for the frame: a cell of FINE_M metres
    f_lat = FINE_M / M_PER_DEG
    f_lon = FINE_M / (M_PER_DEG * k)
    points = geo.sample(FRAME_POINTS, seed=1, bbox=box)
    complete = len(points) < FRAME_POINTS
    w, s0 = box[0], box[1]
    # the fine cells of every address, counted once; the box starts at the south-west, so int() is floor here
    fine: Dict[Tuple[int, int], int] = Counter(
        (int((p["lon"] - w) / f_lon), int((p["lat"] - s0) / f_lat)) for p in points)

    # the coarse grid: whole fine cells, so its cells and the fine ones line up
    long_m = max((box[2] - box[0]) * M_PER_DEG * k, (box[3] - box[1]) * M_PER_DEG)
    per = max(1, math.ceil(long_m / OVERVIEW_CELLS / FINE_M))
    c_lon, c_lat = f_lon * per, f_lat * per
    nx = max(1, math.ceil((box[2] - box[0]) / c_lon))
    ny = max(1, math.ceil((box[3] - box[1]) / c_lat))
    coarse = [0] * (nx * ny)
    for (fx, fy), c in fine.items():
        x, y = fx // per, fy // per
        if 0 <= x < nx and 0 <= y < ny:
            coarse[y * nx + x] += c
    # shade by the logarithm of the count against the busiest cells (99th percentile), so one tower does not wash out
    busy = sorted(c for c in coarse if c)
    top = busy[min(len(busy) - 1, int(len(busy) * 0.99))] if busy else 1
    scale = math.log1p(max(top, 1))
    coarse_bytes = bytes(min(255, round(255 * math.log1p(c) / scale)) if c else 0 for c in coarse)

    # the fine grids: the cells of the lattice around each pin; a sampled frame reads the sheet again around the
    # first NEAR_READS pins, since a sample is too thin for a street
    if not complete:
        fine = Counter()
        seen = set()
        for pin in pins[:NEAR_READS]:
            d_lat, d_lon = (NEAR_M + FINE_M) / M_PER_DEG, (NEAR_M + FINE_M) / (M_PER_DEG * k)
            near = geo.sample(20_000, seed=1, bbox=(pin["lon"] - d_lon, pin["lat"] - d_lat,
                                                    pin["lon"] + d_lon, pin["lat"] + d_lat))
            for p in near:
                key = (p["lon"], p["lat"])
                if key not in seen:
                    seen.add(key)
                    fine[(math.floor((p["lon"] - w) / f_lon), math.floor((p["lat"] - s0) / f_lat))] += 1
    half = NEAR_M // FINE_M
    side = 2 * half
    origins: List[List[float]] = []
    windows = bytearray()
    for pin in pins if complete else pins[:NEAR_READS]:
        x0 = math.floor((pin["lon"] - box[0]) / f_lon) - half
        y0 = math.floor((pin["lat"] - box[1]) / f_lat) - half
        origins.append([round(box[0] + x0 * f_lon, 7), round(box[1] + y0 * f_lat, 7)])
        # a cell with any address is clearly built up; more addresses make it brighter, up to FINE_FULL
        windows += bytes(_fine_shade(fine.get((x0 + i, y0 + j), 0)) for j in range(side) for i in range(side))
    return {
        "overview": {"step": [round(c_lon, 9), round(c_lat, 9)], "size": [nx, ny],
                     "origins": [[round(box[0], 7), round(box[1], 7)]], "cells": _pack(coarse_bytes)},
        "near": {"step": [round(f_lon, 9), round(f_lat, 9)], "size": [side, side], "origins": origins,
                 "cells": _pack(bytes(windows))},
        "complete": complete,
    }


def extension(session: "tools.Session") -> Apps:
    apps = Apps()

    @apps.tool(
        resource_uri=URI,
        name="show_on_map",
        title="Show addresses on a map",
        annotations=ToolAnnotations(title="Show addresses on a map", read_only_hint=True,
                                    destructive_hint=False, open_world_hint=False),
    )
    def show_on_map(queries: List[str], country: str, ctx: Context) -> MapOut:
        """Geocode up to 200 addresses of one country and show them as pins on a map in the chat, offline.
        The base map is the address points of the country sheet; a weak answer is a hollow pin.
        A client without MCP Apps gets the pins as data.

        queries: the addresses, as a person writes them.
        country: ISO code or English name of the country.
        """
        try:
            return map_view(session, queries, country, with_base=client_supports_apps(ctx))
        except tools.ToolError as exc:
            from mcp.server.mcpserver.exceptions import ToolError as McpToolError

            raise McpToolError(str(exc)) from exc

    apps.add_html_resource(URI, MAP_HTML, title="GridPin map", csp=ResourceCsp(connect_domains=[], resource_domains=[]))
    return apps


MAP_HTML = """<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>GridPin map</title>
<style>
:root{--bg:#f7f7f5;--fg:#1f2933;--base:90,108,125;--pin:#c0392b;--weak:#c07a00;--card:#fff;--line:#d9dde2}
@media (prefers-color-scheme: dark){:root{--bg:#171717;--fg:#eceff1;--base:170,182,194;--pin:#ff6b5a;--weak:#f0b429;--card:#232323;--line:#3a3a3a}}
html,body{margin:0;height:100%;background:var(--bg);color:var(--fg);font:13px/1.4 system-ui,sans-serif}
#wrap{position:relative;height:420px}
canvas{display:block;width:100%;height:100%;touch-action:none;cursor:grab}
#bar{position:absolute;top:8px;left:8px;right:8px;display:flex;gap:6px;align-items:center;pointer-events:none}
#bar span{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:2px 8px;pointer-events:auto}
#zoom{position:absolute;right:8px;bottom:8px;display:flex;flex-direction:column;gap:4px}
#zoom button{width:28px;height:28px;border:1px solid var(--line);background:var(--card);color:var(--fg);border-radius:6px;font-size:16px}
#tip{position:absolute;display:none;max-width:260px;background:var(--card);border:1px solid var(--line);border-radius:6px;padding:6px 8px;box-shadow:0 2px 6px rgba(0,0,0,.15)}
</style></head><body>
<div id="wrap"><canvas id="map"></canvas>
<div id="bar"><span id="title">GridPin</span><span id="note" hidden></span></div>
<div id="zoom"><button id="in" aria-label="Zoom in">+</button><button id="out" aria-label="Zoom out">&minus;</button></div>
<div id="tip"></div></div>
<script>
"use strict";
const canvas = document.getElementById("map"), g = canvas.getContext("2d"), tip = document.getElementById("tip");
let data = null, layers = [], view = null, drag = null;
const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
// equirectangular projection around the middle latitude: x = lon * k, y = -lat
function project(lon, lat) { return [lon * view.k, -lat]; }
function toScreen(lon, lat) {
  const [x, y] = project(lon, lat);
  return [(x - view.cx) * view.s + canvas.width / 2, (y - view.cy) * view.s + canvas.height / 2];
}
function fit() {
  const pts = data.pins;
  if (!pts.length) { view = { k: 1, cx: 0, cy: 0, s: 1 }; return; }
  const lats = pts.map((p) => p.lat), lons = pts.map((p) => p.lon);
  const mid = (Math.min(...lats) + Math.max(...lats)) / 2;
  const k = Math.cos(mid * Math.PI / 180);
  const x0 = Math.min(...lons) * k, x1 = Math.max(...lons) * k, y0 = -Math.max(...lats), y1 = -Math.min(...lats);
  const w = Math.max(x1 - x0, 0.004), h = Math.max(y1 - y0, 0.004);
  view = { k, cx: (x0 + x1) / 2, cy: (y0 + y1) / 2, s: 0.8 * Math.min(canvas.width / w, canvas.height / h) };
}
function draw() {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth * dpr, h = canvas.clientHeight * dpr;
  if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; if (data) fit(); }
  g.fillStyle = css("--bg"); g.fillRect(0, 0, canvas.width, canvas.height);
  if (!data) return;
  // the density grids: the coarse one first, then each fine window over a cleared square, so cells stay crisp
  g.imageSmoothingEnabled = false;
  for (const L of layers) {
    const [x0, y0] = toScreen(L.west, L.north), [x1, y1] = toScreen(L.east, L.south);
    // a fine square shows only when its cell is a few pixels wide: before that the coarse grid reads better
    if (L.fine && (x1 - x0) / L.img.width < 2.5) continue;
    if (L.fine) { g.fillStyle = css("--bg"); g.fillRect(x0, y0, x1 - x0, y1 - y0); }
    g.globalAlpha = L.fine ? 0.95 : 0.55; g.drawImage(L.img, x0, y0, x1 - x0, y1 - y0); g.globalAlpha = 1;
  }
  for (const p of data.pins) {
    const [x, y] = toScreen(p.lon, p.lat);
    g.beginPath(); g.arc(x, y, 6 * dpr, 0, 2 * Math.PI);
    g.lineWidth = 2.5 * dpr;
    if (p.weak) { g.strokeStyle = css("--weak"); g.stroke(); }
    else { g.fillStyle = css("--pin"); g.fill(); g.strokeStyle = css("--card"); g.stroke(); }
  }
}
// a grid of the tool result: base64 of zlib bytes, rows from south to north, unpacked into one small canvas per window
async function unpack(text) {
  const bytes = Uint8Array.from(atob(text), (c) => c.charCodeAt(0));
  const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("deflate"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}
async function grids(base) {
  const out = [], rgb = css("--base").split(",").map(Number);
  for (const [grid, fine] of [[base.overview, false], [base.near, true]]) {
    if (!grid || !grid.origins.length) continue;
    const cells = await unpack(grid.cells), [nx, ny] = grid.size, [sx, sy] = grid.step;
    grid.origins.forEach(([west, south], w) => {
      const img = document.createElement("canvas"); img.width = nx; img.height = ny;
      const ctx = img.getContext("2d"), pix = ctx.createImageData(nx, ny);
      for (let j = 0; j < ny; j++) for (let i = 0; i < nx; i++) {
        const v = cells[w * nx * ny + j * nx + i], at = ((ny - 1 - j) * nx + i) * 4;
        pix.data[at] = rgb[0]; pix.data[at + 1] = rgb[1]; pix.data[at + 2] = rgb[2]; pix.data[at + 3] = v;
      }
      ctx.putImageData(pix, 0, 0);
      out.push({ img, fine, west, south, east: west + nx * sx, north: south + ny * sy });
    });
  }
  return out;
}
async function show(result) {
  if (!result || !Array.isArray(result.pins)) return;
  data = result;
  layers = result.base ? await grids(result.base) : [];
  const weak = result.pins.filter((p) => p.weak).length;
  document.getElementById("title").textContent =
    `${result.pins.length} on the map` + (weak ? `, ${weak} weak` : "") + (result.missing.length ? `, ${result.missing.length} not found` : "");
  const note = document.getElementById("note");
  note.hidden = !result.note; note.textContent = result.note || "";
  fit(); draw();
}
function nearest(ev) {
  if (!data) return null;
  const dpr = window.devicePixelRatio || 1, rect = canvas.getBoundingClientRect();
  const mx = (ev.clientX - rect.left) * dpr, my = (ev.clientY - rect.top) * dpr;
  let best = null, bd = 14 * dpr;
  for (const p of data.pins) {
    const [x, y] = toScreen(p.lon, p.lat), d = Math.hypot(x - mx, y - my);
    if (d < bd) { bd = d; best = p; }
  }
  return best;
}
function zoom(f, mx, my) {
  if (!view) return;
  const wx = (mx - canvas.width / 2) / view.s + view.cx, wy = (my - canvas.height / 2) / view.s + view.cy;
  view.s *= f; view.cx = wx - (mx - canvas.width / 2) / view.s; view.cy = wy - (my - canvas.height / 2) / view.s;
  draw();
}
canvas.addEventListener("pointerdown", (e) => { drag = { x: e.clientX, y: e.clientY, moved: false }; canvas.setPointerCapture(e.pointerId); });
canvas.addEventListener("pointermove", (e) => {
  if (!drag || !view) return;
  const dpr = window.devicePixelRatio || 1;
  const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
  if (Math.abs(dx) + Math.abs(dy) > 2) drag.moved = true;
  view.cx -= dx * dpr / view.s; view.cy -= dy * dpr / view.s; drag.x = e.clientX; drag.y = e.clientY; draw();
});
canvas.addEventListener("pointerup", (e) => {
  const click = drag && !drag.moved; drag = null;
  if (!click) return;
  const p = nearest(e);
  if (!p) { tip.style.display = "none"; return; }
  tip.textContent = "";
  const head = document.createElement("b"); head.textContent = p.label; tip.appendChild(head);
  const line = document.createElement("div");
  line.textContent = `${p.precision}${p.weak ? " \\u00b7 weak: check before use" : ""} \\u00b7 ${p.lat.toFixed(6)}, ${p.lon.toFixed(6)}`;
  tip.appendChild(line);
  const asked = document.createElement("div"); asked.textContent = `asked: ${p.query}`; tip.appendChild(asked);
  const rect = canvas.getBoundingClientRect();
  tip.style.left = Math.min(e.clientX - rect.left + 10, rect.width - 270) + "px";
  tip.style.top = (e.clientY - rect.top + 10) + "px";
  tip.style.display = "block";
});
canvas.addEventListener("wheel", (e) => {
  e.preventDefault();
  const dpr = window.devicePixelRatio || 1, rect = canvas.getBoundingClientRect();
  zoom(e.deltaY < 0 ? 1.25 : 0.8, (e.clientX - rect.left) * dpr, (e.clientY - rect.top) * dpr);
}, { passive: false });
document.getElementById("in").onclick = () => zoom(1.5, canvas.width / 2, canvas.height / 2);
document.getElementById("out").onclick = () => zoom(1 / 1.5, canvas.width / 2, canvas.height / 2);
window.addEventListener("resize", draw);
// MCP Apps: the view asks to be initialized, says so, then receives the tool result
let nextId = 1; const pending = new Map();
function post(message) { window.parent.postMessage(message, "*"); }
function request(method, params) {
  const id = nextId++; post({ jsonrpc: "2.0", id, method, params });
  return new Promise((resolve) => pending.set(id, resolve));
}
window.addEventListener("message", (event) => {
  const m = event.data;
  if (!m || m.jsonrpc !== "2.0") return;
  if (m.id !== undefined && pending.has(m.id)) { pending.get(m.id)(m.result); pending.delete(m.id); return; }
  if (m.method === "ui/notifications/tool-result") {
    const r = m.params || {};
    let content = r.structuredContent;
    if (!content && Array.isArray(r.content)) {
      const text = r.content.find((c) => c.type === "text");
      try { content = text && JSON.parse(text.text); } catch (e) { content = null; }
    }
    show(content);
  }
});
request("ui/initialize", {
  appInfo: { name: "gridpin-map", version: "1" },
  appCapabilities: { availableDisplayModes: ["inline"] },
  protocolVersion: "2026-01-26",
}).then(() => {
  post({ jsonrpc: "2.0", method: "ui/notifications/initialized", params: {} });
  post({ jsonrpc: "2.0", method: "ui/notifications/size-changed", params: { height: 420 } });
});
draw();
</script>
</body></html>
"""
