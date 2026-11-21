// The WebAssembly build answers as the CLI does on the Monaco smoke sheet (needs ./build.sh and code/data/mc.bin).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { GridPin, registerModelContextTools } from "../gridpin.js";

const here = (p) => fileURLToPath(new URL(p, import.meta.url));
const WASM = here("../gridpin_wasm.wasm");
const SHEET = process.env.GRIDPIN_TEST_SHEET || here("../../../data/mc.bin");
const CLI = here("../../../gridpin/target/release/gridpin");
const ready = existsSync(WASM) && existsSync(SHEET);

test("geocode and reverse in WebAssembly equal the CLI", { skip: !ready && "run ./build.sh and make mc" }, async () => {
  const gp = await GridPin.load(readFileSync(WASM), readFileSync(SHEET));
  for (const q of ["boulevard des moulins 27, monaco", "avenue de la costa 1 monaco", "xyzzy plugh"]) {
    const ours = gp.geocode(q, 1);
    const cli = execFileSync(CLI, ["query", SHEET, q, "-k", "1"], { stdio: ["ignore", "pipe", "ignore"] }).toString().trim();
    const want = cli ? [JSON.parse(cli)] : [];
    assert.equal(ours.length, want.length, q);
    for (const [a, b] of ours.map((h, i) => [h, want[i]])) {
      for (const key of ["lat", "lon", "street", "housenumber", "commune", "precision", "match_level"]) {
        assert.equal(a[key], b[key], `${q}: ${key}`);
      }
    }
  }
  const r = gp.reverse(43.7406, 7.4246, 1);
  assert.equal(r.length, 1);
  assert.ok(r[0].distance_m >= 0);
});

test("a broken sheet is an error, not a crash", { skip: !existsSync(WASM) && "run ./build.sh" }, async () => {
  await assert.rejects(GridPin.load(readFileSync(WASM), new Uint8Array([1, 2, 3])));
});

test("the npm package gridpin-wasm carries the engine, the loader and the licences, and nothing else", { skip: !existsSync(WASM) && "run ./build.sh" }, async () => {
  const pkg = JSON.parse(readFileSync(here("../package.json")));
  assert.equal(pkg.name, "gridpin-wasm");
  const out = execFileSync("npm", ["pack", "--dry-run", "--json", "--ignore-scripts"], { cwd: here(".."), stdio: ["ignore", "pipe", "ignore"] });
  const files = JSON.parse(out.toString())[0].files.map((f) => f.path).sort();
  assert.deepEqual(files, ["LICENSE", "README.md", "THIRDPARTY.yml", "gridpin.js", "gridpin_wasm.wasm", "package.json"]);
  const { wasmUrl } = await import("../gridpin.js");
  assert.equal(fileURLToPath(wasmUrl), WASM);
});

/** A stand-in for `document.modelContext`: records what the page registers, as the browser would. */
function fakeModelContext() {
  return {
    tools: [],
    async registerTool(tool, options) {
      this.tools.push({ ...tool, signal: options?.signal });
    },
  };
}

test("WebMCP: geocode and reverse are registered once a sheet is loaded and answer as the engine", { skip: !ready && "run ./build.sh and make mc" }, async () => {
  const gp = await GridPin.load(readFileSync(WASM), readFileSync(SHEET));
  const mc = fakeModelContext();
  const signal = new AbortController().signal;
  assert.equal(await registerModelContextTools(gp, { modelContext: mc, signal }), true);
  assert.deepEqual(mc.tools.map((t) => t.name), ["gridpin_geocode", "gridpin_reverse"]);
  assert.equal(mc.tools[0].signal, signal, "the page can remove the tools");
  const [geo, rev] = mc.tools;
  for (const q of ["boulevard des moulins 27, monaco", "avenue de la costa 1 monaco", "xyzzy plugh"]) {
    const out = geo.execute({ query: q, limit: 2 });
    assert.equal(out.isError, undefined, q);
    assert.deepEqual(out.structuredContent.results, gp.geocode(q, 2), q);
    assert.deepEqual(JSON.parse(out.content[0].text), gp.geocode(q, 2), q);
  }
  const r = rev.execute({ lat: 43.7406, lon: 7.4246 });
  assert.deepEqual(r.structuredContent.results, gp.reverse(43.7406, 7.4246, 1));
});

test("WebMCP: every answer keeps weak and precision, and a bad call is the tool's error, not a crash", { skip: !ready && "run ./build.sh and make mc" }, async () => {
  const gp = await GridPin.load(readFileSync(WASM), readFileSync(SHEET));
  const mc = fakeModelContext();
  await registerModelContextTools(gp, { modelContext: mc });
  const [geo, rev] = mc.tools;
  let weak = 0;
  for (const q of ["boulevard des moulins 27, monaco", "monaco", "boulevard des mou 3", "rue grimaldi"]) {
    for (const h of geo.execute({ query: q, limit: 3 }).structuredContent.results) {
      assert.equal(typeof h.weak, "boolean", q);
      assert.equal(typeof h.precision, "string", q);
      weak += h.weak;
    }
  }
  assert.ok(weak > 0, "at least one weak answer among the probes, so the field is seen as true too");
  for (const bad of [{}, { query: "" }, { query: "   " }, { query: "x".repeat(1001) }, { query: 7 }]) {
    const out = geo.execute(bad);
    assert.equal(out.isError, true, JSON.stringify(bad).slice(0, 40));
    assert.ok(out.content[0].text.length > 0);
  }
  for (const bad of [{ lat: 91, lon: 0 }, { lat: 0, lon: 181 }, { lat: "43", lon: 7 }, {}]) {
    assert.equal(rev.execute(bad).isError, true, JSON.stringify(bad));
  }
});

test("WebMCP: no API means nothing registered, and no sheet means no tools", { skip: !existsSync(WASM) && "run ./build.sh" }, async () => {
  const gp = ready ? await GridPin.load(readFileSync(WASM), readFileSync(SHEET)) : null;
  if (gp) assert.equal(await registerModelContextTools(gp, { modelContext: undefined }), false);
  const mc = fakeModelContext();
  await assert.rejects(registerModelContextTools(undefined, { modelContext: mc }), /load a sheet first/);
  assert.equal(mc.tools.length, 0);
});

// Plan T4.2: the suggestions of the engine in the page equal the server's /v1/suggest on the same sheet, field by
// field. The server is a child process killed in `finally`, whatever the test does (PRINCIPLES 4.4).
test("suggest in WebAssembly equals the server's /v1/suggest", { skip: !(ready && existsSync(CLI)) && "run ./build.sh, make mc and build the CLI", timeout: 60_000 }, async () => {
  const { spawn } = await import("node:child_process");
  const gp = await GridPin.load(readFileSync(WASM), readFileSync(SHEET));
  const port = 19_000 + Math.floor(Math.random() * 900);
  const server = spawn(CLI, ["serve", SHEET, "--port", String(port)], { stdio: "ignore" });
  try {
    const base = `http://127.0.0.1:${port}`;
    for (let i = 0; i < 100; i++) {
      try { await fetch(`${base}/v1/suggest?q=boulevard`); break; } catch { await new Promise((r) => setTimeout(r, 100)); }
    }
    const cases = [["boulevard des mou", 5, null], ["27 boulevard des moulins", 3, null], ["avenue", 8, [43.73, 7.42]],
      ["av", 5, null], ["xyzzy plugh", 5, null]];
    for (const [q, k, near] of cases) {
      const url = `${base}/v1/suggest?q=${encodeURIComponent(q)}&k=${k}` + (near ? `&near=${near.join(",")}` : "");
      const want = (await (await fetch(url)).json()).suggestions;
      assert.deepEqual(gp.suggest(q, k, near), want, q);
    }
    assert.ok(gp.suggest("boulevard des mou", 5).length > 0, "the sheet of Monaco has the boulevard");
  } finally {
    server.kill();
  }
});

test("suggest before a sheet is opened is an error, not an empty list", { skip: !existsSync(WASM) && "run ./build.sh" }, async () => {
  const { instance } = await WebAssembly.instantiate(readFileSync(WASM), {});
  const gp = new GridPin(instance.exports);
  assert.throws(() => gp.suggest("boulevard", 5), /no sheet/);
});
