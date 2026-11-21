// <gridpin-address-finder>: what the GOV.UK / DWP address-finder pattern and WCAG 2.2 ask of its markup, read from the
// module itself (no browser here). The run in a real browser with axe-core is finder-a11y.html: zero A and AA findings
// in six states, focus and announcements checked there.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const src = readFileSync(new URL("../gridpin-address-finder.js", import.meta.url), "utf8")
  .split("\n").filter((line) => !line.trimStart().startsWith("//")).join("\n"); // the code, not its comments

test("the list of addresses is radios, not a select", () => {
  assert.match(src, /type="radio"/);
  assert.doesNotMatch(src, /<select/);
});

test("the count of addresses and the server being down are announced", () => {
  assert.match(src, /role="status" aria-live="polite"/);
  assert.match(src, /this\.say\(this\.text\("down"\)\)/);
});

test("the manual fields carry the autocomplete tokens of the HTML standard", () => {
  for (const token of ["address-line1", "address-line2", "address-level2", "postal-code"]) {
    assert.match(src, new RegExp(`autocomplete="${token}"`), token);
  }
});

test("an error is text tied to its field, not only a colour", () => {
  assert.match(src, /setAttribute\("aria-invalid", "true"\)/);
  assert.match(src, /setAttribute\("aria-describedby", box\.id\)/);
  assert.match(src, /`Error: \$\{message\}`/);
});

test("after a search the focus goes to the list's legend, which can take it", () => {
  assert.match(src, /legend id="\$\{id\}-results-legend" tabindex="-1"/);
  assert.match(src, /this\.el\.legend\.focus\(\)/);
});

test("the radios belong to no form: only the address fields are submitted", () => {
  assert.match(src, /type="radio"[^>]*form="\$\{this\.id_\}-none"/);
});

test("a failed request opens the manual fields instead of blocking the form", () => {
  const catchBlock = src.slice(src.indexOf("} catch (e) {"), src.indexOf("this.found = body.addresses"));
  assert.match(catchBlock, /this\.showManual\(false\)/);
});

test("targets are at least 24 CSS pixels (WCAG 2.5.8)", () => {
  assert.match(src, /input\[type=radio\]\{width:24px;height:24px/);
});

test("every text is an attribute with a default", () => {
  const used = [...src.matchAll(/this\.text\("([\w-]+)"/g)].map((m) => m[1]);
  const defaults = src.slice(src.indexOf("const DEFAULTS"), src.indexOf("};", src.indexOf("const DEFAULTS")));
  for (const key of new Set(used)) assert.match(defaults, new RegExp(`(^|\\s|")${key}"?:`), key);
});
