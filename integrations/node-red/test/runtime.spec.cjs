// The node in a real Node-RED runtime (node-red-node-test-helper) against a real `gridpin serve` on the Monaco
// smoke sheet: GRIDPIN_TEST_BIN and GRIDPIN_TEST_SHEET name them (make test-node-red sets both).
"use strict";
const assert = require("node:assert/strict");
const http = require("node:http");
const { spawn } = require("node:child_process");
const helper = require("node-red-node-test-helper");
const gridpinNode = require("../nodes/gridpin.js"); // the node in a real Node-RED runtime; test/gridpin.test.mjs checks its logic in the gate

const BIN = process.env.GRIDPIN_TEST_BIN;
const SHEET = process.env.GRIDPIN_TEST_SHEET;
const HOUSE = "boulevard des moulins 27 monaco";
helper.init(require.resolve("node-red"));

let server;
let base;

function startServer() {
  return new Promise((resolve, reject) => {
    server = spawn(BIN, ["serve", SHEET, "--port", "0"], { stdio: ["ignore", "pipe", "pipe"] });
    let text = "";
    const seen = (chunk) => {
      text += chunk;
      const m = text.match(/listening on (http:\/\/127\.0\.0\.1:\d+)/);
      if (m) resolve(m[1]);
    };
    server.stdout.on("data", seen);
    server.stderr.on("data", seen);
    server.on("exit", (code) => reject(new Error(`gridpin serve exited ${code}: ${text}`)));
  });
}

function flow(url, k = 1) {
  return [
    { id: "g", type: "gridpin", server: url, k, wires: [["out"]] },
    { id: "out", type: "helper" },
  ];
}

function run(url, payload, k = 1) {
  return new Promise((resolve, reject) => {
    helper.load(gridpinNode, flow(url, k), () => {
      const g = helper.getNode("g");
      const out = helper.getNode("out");
      out.on("input", (msg) => resolve(msg));
      g.on("call:error", (call) => reject(call.args[0]));
      g.receive({ payload });
    });
  });
}

async function direct(queries, k = 1) {
  const r = await fetch(`${base}/v1/geocode`, { method: "POST", body: JSON.stringify({ queries, k }) });
  return (await r.json()).results;
}

describe("gridpin node", function () {
  this.timeout(20000);
  before(async () => { base = await startServer(); });
  after(() => { if (server) server.kill(); });
  afterEach(() => helper.unload());

  it("answers one address with the server's own hits", async () => {
    const msg = await run(base, HOUSE);
    assert.deepEqual(msg.payload, (await direct([HOUSE]))[0]);
    assert.equal(msg.payload[0].precision, "house");
    assert.deepEqual(msg.gridpin, { asked: 1, weak: 0, empty: 0 });
  });

  it("answers an array of addresses in one request, in order, and counts weak and empty answers", async () => {
    const asked = [HOUSE, "zzzz qqqq", "monaco", null];
    const want = await direct(asked.map((q) => q || ""), 2);
    const calls = [];
    const real = globalThis.fetch;
    globalThis.fetch = (url, init) => { calls.push([String(url), init && init.method]); return real(url, init); };
    try {
      const msg = await run(base, asked, 2);
      assert.deepEqual(msg.payload, want);
      assert.equal(msg.gridpin.asked, 4);
      assert.equal(msg.gridpin.empty, msg.payload.filter((a) => a.length === 0).length);
      assert.equal(msg.gridpin.weak, msg.payload.flat().filter((h) => h.weak).length);
      assert.ok(msg.gridpin.weak >= 1, "the town alone is a weak answer");
    } finally {
      globalThis.fetch = real;
    }
    assert.equal(calls.filter(([u]) => u.includes("/v1/")).length, 1, "one request for the whole array");
    assert.ok(calls.every(([u]) => !u.includes("moulins")), "no address in a URL");
  });

  it("reads a point as reverse geocoding", async () => {
    const msg = await run(base, { lat: 43.7436, lon: 7.4285 });
    const want = await (await fetch(`${base}/v1/reverse?lat=43.7436&lon=7.4285&k=1`)).json();
    assert.deepEqual(msg.payload, want.results);
    assert.equal(msg.payload[0].street, "Boulevard des Moulins");
  });

  it("says plainly when the server is not running", async () => {
    await assert.rejects(run("http://127.0.0.1:1", HOUSE), /GridPin server not running at http:\/\/127\.0\.0\.1:1/);
  });

  it("passes on the status of an error answer", async () => {
    const bad = http.createServer((req, res) => { res.writeHead(500); res.end("boom"); });
    await new Promise((r) => bad.listen(0, "127.0.0.1", r));
    try {
      await assert.rejects(run(`http://127.0.0.1:${bad.address().port}`, HOUSE), /answered 500: boom/);
    } finally {
      bad.close();
    }
  });

  it("refuses a payload that is neither an address nor a point", async () => {
    await assert.rejects(run(base, 42), /must be an address, an array of addresses or \{lat, lon\}/);
  });
});
