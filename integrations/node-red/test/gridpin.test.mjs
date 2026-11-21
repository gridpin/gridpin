// The logic of the gridpin node without Node-RED (it runs in the gate): what a payload asks, the one request it makes
// to a real `gridpin serve` on the Monaco smoke sheet, the plain errors, and the msg it sends on. The node inside a
// real Node-RED runtime is test/runtime.spec.cjs (make test-node-red).
import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const { questions, ask, shape } = require('../nodes/gridpin.js');
const CODE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
const BIN = path.join(CODE, 'gridpin', 'target', 'release', 'gridpin');
const SHEET = path.join(CODE, 'data', 'mc.bin');
const HOUSE = 'boulevard des moulins 27 monaco';

function serve() {
  return new Promise((resolve, reject) => {
    const proc = spawn(BIN, ['serve', SHEET, '--port', '0'], { stdio: ['ignore', 'pipe', 'pipe'] });
    let text = '';
    const seen = (chunk) => {
      text += chunk;
      const m = text.match(/listening on (http:\/\/127\.0\.0\.1:\d+)/);
      if (m) resolve({ base: m[1], proc });
    };
    proc.stdout.on('data', seen);
    proc.stderr.on('data', seen);
    proc.on('exit', (code) => reject(new Error(`gridpin serve exited ${code}: ${text}`)));
  });
}

test('a payload is read as one address, many addresses, a point, or nothing', () => {
  assert.deepEqual(questions(HOUSE), { kind: 'one', queries: [HOUSE] });
  assert.deepEqual(questions(['a', null, 3]), { kind: 'many', queries: ['a', '', '3'] });
  assert.deepEqual(questions({ lat: '43.7', lon: 7.4 }), { kind: 'point', lat: 43.7, lon: 7.4 });
  for (const bad of [42, null, undefined, { lat: 1 }, { lat: 'x', lon: 2 }]) assert.equal(questions(bad), null);
});

test('the msg it sends: one list for one address or a point, a list per address, weak and empty counted', () => {
  const house = { weak: false };
  const town = { weak: true };
  assert.deepEqual(shape({ kind: 'one', queries: ['a'] }, [[house]]),
    { payload: [house], gridpin: { asked: 1, weak: 0, empty: 0 } });
  assert.deepEqual(shape({ kind: 'many', queries: ['a', 'b', 'c'] }, [[house], [town], []]),
    { payload: [[house], [town], []], gridpin: { asked: 3, weak: 1, empty: 1 } });
  assert.deepEqual(shape({ kind: 'point' }, [town]), { payload: [town], gridpin: { asked: 1, weak: 1, empty: 0 } });
});

test('many addresses go in one POST, never in a URL, and come back as the server answers them', async (t) => {
  const { base, proc } = await serve();
  t.after(() => proc.kill());
  const asked = [HOUSE, 'zzzz qqqq', 'monaco', ''];
  const want = (await (await fetch(`${base}/v1/geocode`, { method: 'POST', body: JSON.stringify({ queries: asked, k: 2 }) })).json()).results;
  const calls = [];
  const counting = (url, init) => { calls.push([String(url), init && init.method]); return fetch(url, init); };
  const got = await ask(base, 2, questions(asked), counting);
  assert.deepEqual(got, want);
  assert.equal(got[0][0].precision, 'house');
  assert.deepEqual(calls.map(([, m]) => m), ['POST']);
  assert.ok(calls.every(([u]) => !u.includes('moulins')));
  const point = await ask(base, 1, questions({ lat: 43.7436, lon: 7.4285 }), fetch);
  assert.equal(point[0].street, 'Boulevard des Moulins');
});

test('a stopped server and an error answer are said plainly', async (t) => {
  await assert.rejects(ask('http://127.0.0.1:1', 1, questions(HOUSE), fetch),
    /GridPin server not running at http:\/\/127\.0\.0\.1:1/);
  const bad = http.createServer((req, res) => { res.writeHead(500); res.end('boom'); });
  await new Promise((r) => bad.listen(0, '127.0.0.1', r));
  t.after(() => bad.close());
  await assert.rejects(ask(`http://127.0.0.1:${bad.address().port}/`, 1, questions(HOUSE), fetch), /answered 500: boom/);
});
