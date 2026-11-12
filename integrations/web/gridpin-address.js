// <gridpin-address>: an address field that suggests streets from your own `gridpin serve` as you type.
//
// One file, no dependencies, a native custom element. The server is yours: no key, no session, no count of
// keystrokes anywhere (the suggestions come from GET /v1/suggest of `gridpin serve`, which keeps nothing).
//
//   <script type="module" src="gridpin-address.js"></script>
//   <gridpin-address endpoint="http://127.0.0.1:8080" min-chars="3" name="address"></gridpin-address>
//
// Attributes: endpoint (the server, default the page's own origin), min-chars (3), k (suggestions, 8),
// placeholder, name (the form field the element submits). The element is a form field itself (form-associated):
// a <form> around it submits the typed or chosen text under `name`, form.reset() clears it, and a <label for> focuses
// it. Browsers without ElementInternals get a hidden input beside it instead. Events: `gridpin-select` with the chosen suggestion in
// `event.detail` ({label, street, commune, lat, lon}). Keyboard: arrows move, Enter chooses, Escape closes.

const STYLE = `
:host{display:block;position:relative;font:inherit}
input{box-sizing:border-box;width:100%;font:inherit;padding:.45em .6em;border:1px solid #9aa5b1;border-radius:6px}
ul{position:absolute;z-index:10;left:0;right:0;margin:2px 0 0;padding:0;list-style:none;background:Canvas;color:CanvasText;
  border:1px solid #9aa5b1;border-radius:6px;max-height:16em;overflow-y:auto}
ul[hidden]{display:none}
li{padding:.4em .6em;cursor:pointer}
li[aria-selected="true"],li:hover{background:Highlight;color:HighlightText}
.commune{opacity:.75}
`;

let counter = 0;

export class GridpinAddress extends HTMLElement {
  static formAssociated = true;

  static get observedAttributes() {
    return ["placeholder", "name"];
  }

  constructor() {
    super();
    const id = `gridpin-address-${++counter}`;
    const root = this.attachShadow({ mode: "open", delegatesFocus: true });
    // The inner input lives in the shadow tree, where no form can see it: the element itself carries the value.
    this.internals = typeof this.attachInternals === "function" ? this.attachInternals() : null;
    this.fallback = null;
    root.innerHTML = `<style>${STYLE}</style>
      <input type="text" role="combobox" aria-autocomplete="list" aria-expanded="false" aria-controls="${id}" autocomplete="off">
      <ul id="${id}" role="listbox" hidden></ul>`;
    this.input = root.querySelector("input");
    this.list = root.querySelector("ul");
    this.items = [];
    this.active = -1;
    this.timer = null;
    this.asked = 0;
    this.input.addEventListener("input", () => {
      this.sync();
      this.schedule();
    });
    this.input.addEventListener("keydown", (e) => this.key(e));
    this.input.addEventListener("blur", () => setTimeout(() => this.close(), 150));
  }

  connectedCallback() {
    if (this.internals || this.fallback) return this.sync();
    this.fallback = document.createElement("input");
    this.fallback.type = "hidden";
    this.fallback.name = this.getAttribute("name") ?? "";
    this.after(this.fallback);
    this.sync();
  }

  disconnectedCallback() {
    this.fallback?.remove();
    this.fallback = null;
  }

  attributeChangedCallback(name, _old, value) {
    if (name === "placeholder") this.input.placeholder = value ?? "";
    if (name === "name" && this.fallback) this.fallback.name = value ?? "";
  }

  // What the form submits under `name`: the text of the field, typed or chosen.
  sync() {
    if (this.internals) this.internals.setFormValue(this.input.value);
    if (this.fallback) this.fallback.value = this.input.value;
  }

  formResetCallback() {
    this.value = "";
    this.close();
  }

  formDisabledCallback(disabled) {
    this.input.disabled = disabled;
  }

  formStateRestoreCallback(state) {
    this.value = typeof state === "string" ? state : "";
  }

  get form() {
    return this.internals ? this.internals.form : (this.fallback?.form ?? null);
  }

  get name() {
    return this.getAttribute("name");
  }

  get value() {
    return this.input.value;
  }

  set value(v) {
    this.input.value = v;
    this.sync();
  }

  get endpoint() {
    return (this.getAttribute("endpoint") || window.location.origin).replace(/\/+$/, "");
  }

  schedule() {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this.fetch(), 150);
  }

  async fetch() {
    const q = this.input.value.trim();
    const min = Number(this.getAttribute("min-chars") || 3);
    if (q.length < min) return this.close();
    const k = Number(this.getAttribute("k") || 8);
    const ticket = ++this.asked;
    let body;
    try {
      const r = await fetch(`${this.endpoint}/v1/suggest?q=${encodeURIComponent(q)}&k=${k}`);
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      body = await r.json();
    } catch (err) {
      this.dispatchEvent(new CustomEvent("gridpin-error", { detail: String(err) }));
      return this.close();
    }
    if (ticket !== this.asked) return; // a newer keystroke already asked
    this.show(body.suggestions || []);
  }

  show(items) {
    this.items = items;
    this.active = -1;
    this.list.replaceChildren(
      ...items.map((s, i) => {
        const li = document.createElement("li");
        li.id = `${this.list.id}-${i}`;
        li.setAttribute("role", "option");
        li.setAttribute("aria-selected", "false");
        // The label carries the house number («233 South Wacker Drive, Chicago»); the town is dimmed after it.
        const tail = s.commune ? `, ${s.commune}` : "";
        const main = s.label && tail && s.label.endsWith(tail) ? s.label.slice(0, -tail.length) : s.label || s.street;
        const street = document.createElement("span");
        street.textContent = main;
        const commune = document.createElement("span");
        commune.className = "commune";
        commune.textContent = s.label && !s.label.endsWith(tail) ? "" : tail;
        li.append(street, commune);
        li.addEventListener("mousedown", (e) => {
          e.preventDefault();
          this.choose(i);
        });
        return li;
      }),
    );
    const open = items.length > 0;
    this.list.hidden = !open;
    this.input.setAttribute("aria-expanded", String(open));
  }

  close() {
    this.list.hidden = true;
    this.input.setAttribute("aria-expanded", "false");
    this.input.removeAttribute("aria-activedescendant");
    this.active = -1;
  }

  move(by) {
    if (!this.items.length) return;
    this.active = (this.active + by + this.items.length) % this.items.length;
    [...this.list.children].forEach((li, i) => li.setAttribute("aria-selected", String(i === this.active)));
    const li = this.list.children[this.active];
    this.input.setAttribute("aria-activedescendant", li.id);
    li.scrollIntoView({ block: "nearest" });
  }

  key(e) {
    if (e.key === "ArrowDown") this.move(1);
    else if (e.key === "ArrowUp") this.move(-1);
    else if (e.key === "Enter" && this.active >= 0) this.choose(this.active);
    // Enter with no suggestion highlighted submits the form, as in a plain input: the shadow tree hides the input
    // from the browser's implicit submission.
    else if (e.key === "Enter" && this.form && typeof this.form.requestSubmit === "function") this.form.requestSubmit();
    else if (e.key === "Escape") this.close();
    else return;
    e.preventDefault();
  }

  choose(i) {
    const s = this.items[i];
    if (!s) return;
    this.value = s.label;
    this.close();
    this.dispatchEvent(new CustomEvent("gridpin-select", { detail: s, bubbles: true, composed: true }));
  }
}

if (!customElements.get("gridpin-address")) customElements.define("gridpin-address", GridpinAddress);
