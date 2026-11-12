// <gridpin-address> is a form field: what a <form> around it submits, resets and shows.
// The bug this guards (11.11.2026): the input sat in the shadow tree, no form could see it, and the form sent nothing
// after a suggestion was chosen. A minimal DOM stand-in runs the element under node --test without a browser.
import { test } from "node:test";
import assert from "node:assert/strict";

class Node_ {
  constructor(tag) {
    Object.assign(this, { tag, attrs: {}, children: [], listeners: {}, value: "", hidden: false, textContent: "" });
  }
  setAttribute(k, v) { this.attrs[k] = String(v); }
  getAttribute(k) { return this.attrs[k] ?? null; }
  removeAttribute(k) { delete this.attrs[k]; }
  addEventListener(t, f) { (this.listeners[t] ||= []).push(f); }
  fire(t, e = {}) { for (const f of this.listeners[t] || []) f({ preventDefault() {}, ...e }); }
  append(...c) { this.children.push(...c); }
  replaceChildren(...c) { this.children = c; }
  get text() { return this.textContent + this.children.map((c) => c.text).join(""); }
  scrollIntoView() {}
}

const placed = [];
globalThis.document = { createElement: (tag) => new Node_(tag) };
globalThis.window = { location: { origin: "http://127.0.0.1:8080" } };
globalThis.CustomEvent = class { constructor(type, init) { Object.assign(this, { type }, init); } };
globalThis.customElements = { get: () => undefined, define() {} };
let withInternals = true;
globalThis.HTMLElement = class {
  constructor() {
    this.attrs = {};
    this.events = [];
    if (withInternals) {
      const form = { submits: 0, requestSubmit() { this.submits += 1; } };
      this.attachInternals = () => ({ form, submitted: undefined, setFormValue(v) { this.submitted = v; } });
    }
  }
  attachShadow(opts) {
    this.shadowOpts = opts;
    const input = new Node_("input"), list = new Node_("ul");
    list.id = "list";
    return { set innerHTML(_) {}, querySelector: (s) => (s === "input" ? input : list) };
  }
  setAttribute(k, v) { this.attrs[k] = String(v); }
  getAttribute(k) { return this.attrs[k] ?? null; }
  dispatchEvent(e) { this.events.push(e); }
  after(n) { placed.push(n); }
};

const { GridpinAddress } = await import("../gridpin-address.js");
const CHICAGO = { label: "233 South WACKER Drive, Chicago", street: "South WACKER Drive", commune: "Chicago", lat: 41.88, lon: -87.64 };

function field(attrs = { name: "address" }) {
  const el = new GridpinAddress();
  for (const [k, v] of Object.entries(attrs)) {
    el.setAttribute(k, v);
    el.attributeChangedCallback(k, null, v);
  }
  el.connectedCallback();
  return el;
}

test("the element declares itself a form field and lets a label focus it", () => {
  assert.equal(GridpinAddress.formAssociated, true);
  assert.equal(field().shadowOpts.delegatesFocus, true);
});

test("a chosen suggestion is what the form submits", () => {
  const el = field();
  el.show([CHICAGO]);
  el.list.children[0].fire("mousedown");
  assert.equal(el.internals.submitted, CHICAGO.label);
  assert.equal(el.value, CHICAGO.label);
  assert.equal(el.events.at(-1).type, "gridpin-select");
  assert.deepEqual(el.events.at(-1).detail, CHICAGO);
});

test("typed text without a choice is submitted as typed", () => {
  const el = field();
  el.input.value = "12 rue inconnue";
  el.input.fire("input");
  clearTimeout(el.timer);
  assert.equal(el.internals.submitted, "12 rue inconnue");
});

test("form.reset() empties the field and what it submits", () => {
  const el = field();
  el.value = CHICAGO.label;
  el.formResetCallback();
  assert.equal(el.value, "");
  assert.equal(el.internals.submitted, "");
});

test("a disabled fieldset disables the inner input", () => {
  const el = field();
  el.formDisabledCallback(true);
  assert.equal(el.input.disabled, true);
});

test("without ElementInternals a hidden input beside the element carries the value under the name", () => {
  withInternals = false;
  try {
    const el = field({ name: "address" });
    const hidden = placed.at(-1);
    assert.equal(el.internals, null);
    assert.equal(hidden.type, "hidden");
    assert.equal(hidden.name, "address");
    el.show([CHICAGO]);
    el.list.children[0].fire("mousedown");
    assert.equal(hidden.value, CHICAGO.label);
  } finally {
    withInternals = true;
  }
});

test("a suggestion row shows the house number, with the town after it", () => {
  const el = field();
  el.show([CHICAGO]);
  const [main, town] = el.list.children[0].children;
  assert.equal(main.textContent, "233 South WACKER Drive");
  assert.equal(town.textContent, ", Chicago");
});

test("Enter submits the form when no suggestion is highlighted, and chooses when one is", () => {
  const el = field();
  el.show([CHICAGO]);
  el.input.fire("keydown", { key: "ArrowDown" });
  el.input.fire("keydown", { key: "Enter" });
  assert.equal(el.internals.form.submits, 0);
  assert.equal(el.value, CHICAGO.label);
  el.input.fire("keydown", { key: "Enter" });
  assert.equal(el.internals.form.submits, 1);
});
