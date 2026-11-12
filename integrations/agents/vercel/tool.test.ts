// Run with the AI SDK and zod installed next to it (see README): node --test tool.test.ts
import test from 'node:test';
import assert from 'node:assert/strict';
import { gridpinTools } from './tool.ts';

const SHEET = process.env.GRIDPIN_TEST_SHEET ?? '../../../data/mc.bin';
const ADDRESS = 'boulevard des moulins 27 monaco';

test('the geocode tool answers what the Node package answers and validates its input', async () => {
  const tools = gridpinTools(SHEET);
  const out = await tools.gridpin_geocode.execute!({ query: ADDRESS, k: 1 }, { toolCallId: '1', messages: [] } as any);
  assert.equal(out.results[0].housenumber, '27');
  const parsed = (tools.gridpin_geocode.inputSchema as any).safeParse({ query: ADDRESS, k: 11 });
  assert.equal(parsed.success, false, 'k above 10 is refused before the engine');
  const top = out.results[0];
  const back = await tools.gridpin_reverse.execute!({ lat: top.lat, lon: top.lon, k: 1 }, { toolCallId: '2', messages: [] } as any);
  assert.equal(back.results[0].housenumber, '27');
});
