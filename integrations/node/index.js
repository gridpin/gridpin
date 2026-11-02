'use strict';
// GridPin for Node.js: an offline geocoder over one memory-mapped sheet per country.
// The native part (gridpin.node) is the GridPin C interface, the same one the R package and the DuckDB
// extension use; answers come back as the same JSON the CLI prints.
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');

const OP = { load: 0, loadPoi: 1, reset: 2, geocode: 3 };

function loadNative() {
  // build.sh writes build/gridpin.node, node-gyp (binding.gyp) build/Release/gridpin.node
  for (const local of [path.join(__dirname, 'build', 'gridpin.node'), path.join(__dirname, 'build', 'Release', 'gridpin.node')]) {
    if (fs.existsSync(local)) return require(local);
  }
  const name = `@gridpin/gridpin-${process.platform}-${process.arch}`;
  try {
    return require(name);
  } catch (e) {
    throw new Error(`gridpin: no prebuilt binary for ${process.platform}-${process.arch} (${name}); ` +
      'from a GridPin source checkout, integrations/node/build.sh builds build/gridpin.node');
  }
}
const native = loadNative();

class Geocoder {
  /** Open a sheet (.bin); `poi` is an optional POI layer of the same country. */
  constructor(sheet, { poi } = {}) {
    this._state = native.state();
    native.text(this._state, OP.load, 0, [String(sheet)]);
    if (poi) native.text(this._state, OP.loadPoi, 0, [String(poi)]);
  }

  _live() {
    if (!this._state) throw new Error('gridpin: the geocoder was closed; open it again');
    return this._state;
  }

  /** Up to k answers for one address, best first; [] when nothing matches. */
  geocode(query, { k = 10 } = {}) {
    return this.geocodeMany([query], { k })[0];
  }

  /** One list of up to k answers per query, in input order; null or undefined queries give []. */
  geocodeMany(queries, { k = 1 } = {}) {
    if (!Number.isInteger(k) || k < 1) throw new RangeError('gridpin: k must be an integer >= 1');
    const rows = native.text(this._live(), OP.geocode, k, Array.from(queries, (q) => (q == null ? null : String(q))));
    return rows.map((row) => (row == null ? [] : JSON.parse(row)));
  }

  /** Up to k nearest addresses to a WGS84 point. */
  reverse(lat, lon, { k = 1 } = {}) {
    if (!Number.isInteger(k) || k < 1) throw new RangeError('gridpin: k must be an integer >= 1');
    const [row] = native.reverse(this._live(), k, [Number(lat)], [Number(lon)]);
    return row == null ? [] : JSON.parse(row);
  }

  /** Unmap the sheet now instead of at garbage collection. */
  close() {
    if (this._state) native.text(this._state, OP.reset, 0, []);
    this._state = null;
  }
}

// Sheets of the release (sheets.json, a copy of gridpin/sheets.json kept equal by the repository checks).
// Same folder as the CLI and gridpin-mcp: GRIDPIN_HOME, else $XDG_CACHE_HOME/gridpin, else ~/.cache/gridpin.
const REGISTRY = JSON.parse(fs.readFileSync(path.join(__dirname, 'sheets.json'), 'utf8'));

function home() {
  const env = process.env;
  const tilde = (p) => (p.startsWith('~') ? path.join(os.homedir(), p.slice(1)) : p);
  if (env.GRIDPIN_HOME) return tilde(env.GRIDPIN_HOME);
  if (env.XDG_CACHE_HOME) return path.join(tilde(env.XDG_CACHE_HOME), 'gridpin');
  return path.join(os.homedir(), '.cache', 'gridpin');
}

function resolve(country) {
  const key = String(country || '').trim().toLowerCase();
  const sheet = REGISTRY.sheets.find((s) => s.code === key || s.name.toLowerCase() === key);
  if (!sheet) {
    const known = REGISTRY.sheets.map((s) => `${s.code} (${s.name})`).join(', ');
    throw new Error(`gridpin: unknown country ${JSON.stringify(country)}; available: ${known}`);
  }
  return sheet;
}

const localPath = (sheet) => path.join(home(), REGISTRY.release, sheet.file);
const urlOf = (sheet) => `${REGISTRY.base_url}/${REGISTRY.release}/${sheet.file}`;

function sha256(file) {
  const hash = crypto.createHash('sha256');
  const fd = fs.openSync(file, 'r');
  const block = Buffer.alloc(1 << 20);
  try {
    let n;
    while ((n = fs.readSync(fd, block, 0, block.length, null)) > 0) hash.update(block.subarray(0, n));
  } finally {
    fs.closeSync(fd);
  }
  return hash.digest('hex');
}

/** 'ok' | 'missing' | 'wrong size' | 'wrong sha256' for a file against a sheet of the registry. */
function verify(file, sheet) {
  let size;
  try {
    size = fs.statSync(file).size;
  } catch {
    return 'missing';
  }
  if (size !== sheet.size) return 'wrong size';
  return sha256(file) === sheet.sha256 ? 'ok' : 'wrong sha256';
}

const sheets = {
  release: REGISTRY.release,
  list: () => REGISTRY.sheets.map((s) => {
    const p = localPath(s);
    let downloaded = false;
    try { downloaded = fs.statSync(p).size === s.size; } catch { /* not downloaded */ }
    return { ...s, url: urlOf(s), path: p, downloaded };
  }),
  where: (country) => (country === undefined ? path.join(home(), REGISTRY.release) : localPath(resolve(country))),
  verify: (country) => verify(localPath(resolve(country)), resolve(country)),

  /** Download a sheet once (fetch, then size and SHA-256 against the signed release); returns its path. */
  async fetch(country, { offline = ['1', 'true', 'yes'].includes(String(process.env.GRIDPIN_OFFLINE || '').toLowerCase()) } = {}) {
    const sheet = resolve(country);
    const target = localPath(sheet);
    if (verify(target, sheet) === 'ok') return target;
    if (offline) throw new Error(`gridpin: offline, and the ${sheet.name} sheet is not downloaded: ${urlOf(sheet)} -> ${target}`);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    const part = `${target}.${process.pid}.part`;
    try {
      const response = await fetch(urlOf(sheet));
      if (!response.ok) throw new Error(`gridpin: ${urlOf(sheet)} answered ${response.status}`);
      const out = fs.createWriteStream(part);
      for await (const chunk of response.body) {
        if (!out.write(chunk)) await new Promise((r) => out.once('drain', r));
      }
      await new Promise((r, j) => out.end((e) => (e ? j(e) : r())));
      const verdict = verify(part, sheet);
      if (verdict !== 'ok') throw new Error(`gridpin: ${urlOf(sheet)}: ${verdict}; not used`);
      fs.renameSync(part, target);
      return target;
    } finally {
      fs.rmSync(part, { force: true });
    }
  },
};

module.exports = { Geocoder, sheets, abiVersion: () => native.abi() };
