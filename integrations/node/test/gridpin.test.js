// Against the Monaco smoke sheet (code/data/mc.bin, `make mc`) and the CLI built from the same tree:
// every answer is compared with what `gridpin query` prints for the same text, not with a hand-written value.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const { Geocoder, sheets } = createRequire(import.meta.url)('..');

const CODE = path.resolve(__dirname, '..', '..', '..');
const SHEET = process.env.GRIDPIN_TEST_SHEET || path.join(CODE, 'data', 'mc.bin');
const CLI = path.join(CODE, 'gridpin', 'target', 'release', 'gridpin');

function cli(query, k) {
  const out = execFileSync(CLI, ['query', SHEET, query, '-k', String(k)], { encoding: 'utf8' });
  return out.split('\n').filter(Boolean).map((line) => JSON.parse(line));
}

function same(hit, expected) {
  assert.deepStrictEqual(
    [hit.lat, hit.lon, hit.precision, hit.street, hit.housenumber, hit.commune, hit.weak],
    [expected.lat, expected.lon, expected.precision, expected.street, expected.housenumber, expected.commune, expected.weak],
  );
}

test('geocode answers what the CLI answers, k honoured', () => {
  const g = new Geocoder(SHEET);
  for (const q of ['boulevard des moulins 27 monaco', 'monaco']) {
    const ours = g.geocode(q, { k: 3 });
    const theirs = cli(q, 3);
    assert.strictEqual(ours.length, theirs.length, q);
    ours.forEach((hit, i) => same(hit, theirs[i]));
  }
  assert.strictEqual(g.geocode('boulevard des moulins 27 monaco', { k: 1 }).length, 1);
  assert.strictEqual(g.geocode('monaco')[0].weak, true);
  assert.deepStrictEqual(g.geocode('xyzzy plugh nonexistent street 999'), []);
});

test('geocodeMany keeps input order and gives [] for null rows', () => {
  const g = new Geocoder(SHEET);
  const queries = ['monaco', null, 'boulevard des moulins 27 monaco', undefined];
  const rows = g.geocodeMany(queries);
  assert.strictEqual(rows.length, 4);
  same(rows[0][0], cli('monaco', 1)[0]);
  assert.deepStrictEqual(rows[1], []);
  same(rows[2][0], cli('boulevard des moulins 27 monaco', 1)[0]);
  assert.deepStrictEqual(rows[3], []);
  assert.throws(() => g.geocodeMany(['monaco'], { k: 0 }), RangeError);
});

test('reverse finds the house the forward answer points at', () => {
  const g = new Geocoder(SHEET);
  const house = g.geocode('boulevard des moulins 27 monaco', { k: 1 })[0];
  const [back] = g.reverse(house.lat, house.lon);
  assert.strictEqual(back.housenumber, house.housenumber);
  assert.strictEqual(back.street, house.street);
});

test('a closed geocoder refuses, a missing sheet is an error that names it', () => {
  const g = new Geocoder(SHEET);
  g.close();
  assert.throws(() => g.geocode('monaco'), /closed/);
  assert.throws(() => new Geocoder(path.join(os.tmpdir(), 'no-such-sheet.bin')), /no-such-sheet/);
});

test('sheets: same folder rule as the CLI, a wrong file is refused, offline fetch does not touch the network', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'gridpin-node-'));
  const saved = { ...process.env };
  try {
    for (const vars of [{ HOME: dir }, { HOME: dir, XDG_CACHE_HOME: path.join(dir, 'xdg') }, { HOME: dir, GRIDPIN_HOME: path.join(dir, 'own') }]) {
      delete process.env.GRIDPIN_HOME;
      delete process.env.XDG_CACHE_HOME;
      Object.assign(process.env, vars);
      const cliWhere = execFileSync(CLI, ['sheets', 'where'], { encoding: 'utf8', env: process.env }).trim();
      assert.strictEqual(sheets.where(), cliWhere, JSON.stringify(vars));
    }
    process.env.GRIDPIN_HOME = dir;
    const listed = JSON.parse(execFileSync(CLI, ['sheets', 'list', '--json'], { encoding: 'utf8', env: process.env }));
    assert.deepStrictEqual(sheets.list().map((s) => [s.code, s.path, s.url, s.sha256]),
      listed.map((s) => [s.code, s.path, s.url, s.sha256]));
    const smallest = sheets.list().reduce((a, b) => (a.size < b.size ? a : b));
    fs.mkdirSync(path.dirname(smallest.path), { recursive: true });
    fs.writeFileSync(smallest.path, 'not a sheet');
    assert.strictEqual(sheets.verify(smallest.code), 'wrong size');
    fs.rmSync(smallest.path);
    await assert.rejects(sheets.fetch(smallest.code, { offline: true }), /offline/);
    assert.strictEqual(fs.existsSync(smallest.path), false);
  } finally {
    process.env = saved;
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
