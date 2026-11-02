// A sheet built here with `gridpin build` from three rows, so this file runs anywhere the CLI and the module
// build, Windows included (no smoke-sheet pipeline needed). Answers are compared with `gridpin query`.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const { Geocoder } = createRequire(import.meta.url)('..');
const CODE = path.resolve(HERE, '..', '..', '..');
const CLI = path.join(CODE, 'gridpin', 'target', 'release', process.platform === 'win32' ? 'gridpin.exe' : 'gridpin');

function sheet() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'gridpin-node-fixture-'));
  const csv = path.join(dir, 'in.csv');
  fs.writeFileSync(csv, 'nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n' +
    'rue a,001,ville,10000,1,,7.420,43.730,Rue A,Ville\nrue a,001,ville,10000,3,,7.421,43.731,Rue A,Ville\n' +
    'rue b,001,ville,10000,1,,7.425,43.735,Rue B,Ville\n');
  const manifest = path.join(dir, 'm.json');
  fs.writeFileSync(manifest, '{"country":"mc","layer":"addresses","license":"t","source_release":"test"}');
  const bin = path.join(dir, 's.bin');
  execFileSync(CLI, ['build', csv, bin, '--meta', manifest]);
  return bin;
}

const cli = (bin, q) => execFileSync(CLI, ['query', bin, q, '-k', '1'], { encoding: 'utf8' })
  .split('\n').filter(Boolean).map((l) => JSON.parse(l));

test('the module answers like the CLI on a sheet built here, forward, batch and reverse', () => {
  const bin = sheet();
  const g = new Geocoder(bin);
  for (const q of ['rue a 3 ville', 'rue b 1 ville']) {
    const [ours] = g.geocode(q, { k: 1 });
    const [theirs] = cli(bin, q);
    assert.deepEqual([ours.lat, ours.lon, ours.housenumber, ours.street, ours.weak],
      [theirs.lat, theirs.lon, theirs.housenumber, theirs.street, theirs.weak], q);
  }
  const rows = g.geocodeMany(['rue b 1 ville', null, 'rue a 1 ville']);
  assert.deepEqual(rows.map((r) => r[0]?.street ?? null), ['Rue B', null, 'Rue A']);
  const [back] = g.reverse(43.731, 7.421);
  assert.equal(back.housenumber, '3');
  g.close();
});
