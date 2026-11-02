// The release packages: one per platform with os/cpu, the main one pinned to all of them, nothing missing.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { assemble, PLATFORMS, packageName } from '../scripts/platform-packages.mjs';

function natives(keys) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'gridpin-natives-'));
  for (const k of keys) {
    fs.mkdirSync(path.join(dir, k));
    fs.writeFileSync(path.join(dir, k, 'gridpin.node'), `module for ${k}`);
  }
  return dir;
}

const ALL = PLATFORMS.map((p) => `${p.platform}-${p.arch}`);

test('every platform gets its package and the main package depends on each at the same version', () => {
  const out = fs.mkdtempSync(path.join(os.tmpdir(), 'gridpin-packages-'));
  assemble(natives(ALL), out, '9.8.7');
  const main = JSON.parse(fs.readFileSync(path.join(out, 'gridpin', 'package.json'), 'utf8'));
  assert.equal(main.name, 'gridpin');
  assert.equal(main.version, '9.8.7');
  assert.deepEqual(Object.keys(main.optionalDependencies).sort(), PLATFORMS.map(packageName).sort());
  assert.ok(Object.values(main.optionalDependencies).every((v) => v === '9.8.7'));
  for (const p of PLATFORMS) {
    const dir = path.join(out, `${p.platform}-${p.arch}`);
    const pkg = JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8'));
    assert.deepEqual([pkg.name, pkg.version, pkg.os, pkg.cpu], [packageName(p), '9.8.7', [p.platform], [p.arch]]);
    assert.equal(fs.readFileSync(path.join(dir, 'gridpin.node'), 'utf8'), `module for ${p.platform}-${p.arch}`);
    for (const f of pkg.files) assert.ok(fs.existsSync(path.join(dir, f)), `${pkg.name}: ${f}`);
    assert.match(fs.readFileSync(path.join(dir, 'THIRDPARTY.yml'), 'utf8'), /serde/);
  }
  for (const f of main.files) assert.ok(fs.existsSync(path.join(out, 'gridpin', f)), f);
});

test('a missing or an unexpected module stops the release, and so does a bad version', () => {
  const out = fs.mkdtempSync(path.join(os.tmpdir(), 'gridpin-packages-'));
  assert.throws(() => assemble(natives(ALL.slice(1)), out, '1.0.0'), /missing/);
  assert.throws(() => assemble(natives([...ALL, 'freebsd-x64']), out, '1.0.0'), /unexpected/);
  assert.throws(() => assemble(natives(ALL), out, 'v1.0.0'), /X\.Y\.Z/);
});
