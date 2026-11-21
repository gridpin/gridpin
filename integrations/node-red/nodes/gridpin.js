// GridPin geocoder node: msg.payload goes to a local `gridpin serve`, the answers come back in msg.payload.
//   a string        -> one address  -> an array of up to k hits
//   an array        -> many addresses, one POST -> one array of hits per address (null stays null)
//   {lat, lon}      -> a point (reverse) -> an array of up to k nearest addresses
// Addresses travel only in the body of a POST, never in a URL. msg.gridpin says how many answers are weak.
"use strict";

function questions(payload) {
  if (typeof payload === "string") return { kind: "one", queries: [payload] };
  if (Array.isArray(payload)) return { kind: "many", queries: payload.map((q) => (q == null ? "" : String(q))) };
  if (payload && typeof payload === "object" && isFinite(payload.lat) && isFinite(payload.lon)) {
    return { kind: "point", lat: Number(payload.lat), lon: Number(payload.lon) };
  }
  return null;
}

async function ask(server, k, q, fetchImpl) {
  const base = server.replace(/\/+$/, "");
  let response;
  try {
    response = q.kind === "point"
      ? await fetchImpl(`${base}/v1/reverse?lat=${q.lat}&lon=${q.lon}&k=${k}`)
      : await fetchImpl(`${base}/v1/geocode`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ queries: q.queries, k }),
        });
  } catch (e) {
    throw new Error(`GridPin server not running at ${base} (start it with: gridpin serve <sheet>.bin)`);
  }
  if (!response.ok) {
    let detail = "";
    try { detail = (await response.text()).slice(0, 200); } catch (e) { /* no body */ }
    throw new Error(`GridPin server at ${base} answered ${response.status}${detail ? ": " + detail : ""}`);
  }
  const body = await response.json();
  return body.results;
}

function weakCount(answers) {
  return answers.flat().filter((h) => h && h.weak).length;
}

/** msg.payload and msg.gridpin for the server's results: one address and a point give one list of hits, many
 *  addresses one list per address. */
function shape(q, results) {
  const payload = q.kind === "one" ? results[0] : results;
  const list = q.kind === "many" ? results : [payload];
  return {
    payload,
    gridpin: { asked: q.kind === "many" ? q.queries.length : 1, weak: weakCount(list),
               empty: list.filter((a) => !a || a.length === 0).length },
  };
}

module.exports = function (RED) {
  function GridPinNode(config) {
    RED.nodes.createNode(this, config);
    const node = this;
    const server = config.server || "http://127.0.0.1:8080";
    const k = Math.max(1, parseInt(config.k, 10) || 1);
    node.on("input", async (msg, send, done) => {
      const q = questions(msg.payload);
      if (!q) {
        done(new Error("msg.payload must be an address, an array of addresses or {lat, lon}"));
        return;
      }
      node.status({ fill: "blue", shape: "dot", text: "asking" });
      try {
        const answer = shape(q, await ask(server, k, q, globalThis.fetch));
        msg.payload = answer.payload;
        msg.gridpin = answer.gridpin;
        node.status({});
        send(msg);
        done();
      } catch (e) {
        node.status({ fill: "red", shape: "ring", text: "error" });
        done(e);
      }
    });
  }
  RED.nodes.registerType("gridpin", GridPinNode);
};
module.exports.questions = questions;
module.exports.ask = ask;
module.exports.shape = shape;
