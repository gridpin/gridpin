// The WebAssembly build answers as the CLI does on the Monaco smoke sheet (needs ./build.sh and code/data/mc.bin).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { GridPin } from "../gridpin.js";

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
