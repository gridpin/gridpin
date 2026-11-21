// <gridpin-address-finder>: find an address by postcode (and house number), choose it from a list, or type it by hand.
//
// One file, no dependencies, a native custom element in the light DOM, so the form around it submits plain fields and
// assistive technology sees ordinary labels, radios and inputs. The pattern is the GOV.UK and DWP address finder:
// postcode → a list of radios (not a <select>) → "Use this address"; typing the address by hand is offered on every
// step. The addresses come from GET /v1/addresses of your own `gridpin serve`; nothing else is asked.
//
//   <script type="module" src="gridpin-address-finder.js"></script>
//   <gridpin-address-finder endpoint="http://127.0.0.1:8080" name-prefix="delivery-"></gridpin-address-finder>
//
// The list of radios belongs to no form (form="…-none"): only the address fields are submitted. The form gets <prefix>address-line1, <prefix>address-line2, <prefix>city and <prefix>postcode, with the
// autocomplete tokens of the HTML standard. Texts are attributes (text-legend, text-postcode, text-number, text-find,
// text-use, text-manual, text-found, text-none, text-more, text-down, text-error-postcode, text-line1, text-line2,
// text-city): pages in other languages set them. Event `gridpin-address` with the chosen address in `event.detail`.

const DEFAULTS = {
  legend: "Address",
  postcode: "Postcode",
  number: "House number, or street and number (optional)",
  find: "Find address",
  use: "Use this address",
  manual: "Enter the address manually",
  found: "{n} addresses found.",
  one: "1 address found.",
  none: "No address found for {postcode}. Check the postcode or enter the address manually.",
  more: "Showing the first {n}: add the house number to narrow the list.",
  "more-number": "Showing the first {n}: if yours is not there, enter the address manually.",
  down: "The address finder is not available. Enter the address manually.",
  "error-postcode": "Enter a postcode",
  "error-choose": "Choose an address",
  line1: "Address line 1",
  line2: "Address line 2 (optional)",
  city: "Town or city",
  pick: "Choose your address",
};

// Sizes only, so a page without its own styles still meets WCAG 2.5.8 (targets of 24 × 24 CSS pixels at least);
// colours, fonts and spacing stay the page's.
const STYLE = `.gp-finder input{min-height:44px;box-sizing:border-box}
.gp-finder input[type=radio]{width:24px;height:24px;min-height:24px;margin:0 .5em 0 0;vertical-align:middle}
.gp-finder button{min-height:44px;min-width:44px}
.gp-finder .gp-radio{min-height:44px;display:flex;align-items:center}
.gp-finder [hidden]{display:none}`;

let counter = 0;

const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

export class GridpinAddressFinder extends HTMLElement {
  text(key, vars = {}) {
    const raw = this.getAttribute(`text-${key}`) ?? DEFAULTS[key];
    return raw.replace(/\{(\w+)\}/g, (_, k) => (k in vars ? String(vars[k]) : ""));
  }

  connectedCallback() {
    if (this.built) return;
    this.built = true;
    if (!document.getElementById("gp-finder-style")) {
      const style = document.createElement("style");
      style.id = "gp-finder-style";
      style.textContent = STYLE;
      document.head.append(style);
    }
    const id = `gp-finder-${++counter}`;
    const p = this.getAttribute("name-prefix") || "";
    this.id_ = id;
    this.innerHTML = `
<fieldset class="gp-finder" aria-describedby="${id}-status">
  <legend>${esc(this.text("legend"))}</legend>
  <div class="gp-step" id="${id}-find">
    <div class="gp-field">
      <label for="${id}-postcode">${esc(this.text("postcode"))}</label>
      <p class="gp-error" id="${id}-postcode-error" hidden></p>
      <input id="${id}-postcode" type="text" autocomplete="postal-code" spellcheck="false">
    </div>
    <div class="gp-field">
      <label for="${id}-number">${esc(this.text("number"))}</label>
      <input id="${id}-number" type="text" autocomplete="off">
    </div>
    <button type="button" id="${id}-go">${esc(this.text("find"))}</button>
  </div>
  <p role="status" aria-live="polite" id="${id}-status" class="gp-status"></p>
  <fieldset id="${id}-results" hidden>
    <legend id="${id}-results-legend" tabindex="-1">${esc(this.text("pick"))}</legend>
    <p class="gp-error" id="${id}-choose-error" hidden></p>
    <div id="${id}-list"></div>
    <button type="button" id="${id}-use">${esc(this.text("use"))}</button>
  </fieldset>
  <p><button type="button" class="gp-link" id="${id}-manual-link">${esc(this.text("manual"))}</button></p>
  <div id="${id}-manual" hidden>
    <div class="gp-field"><label for="${id}-line1">${esc(this.text("line1"))}</label>
      <input id="${id}-line1" name="${p}address-line1" autocomplete="address-line1"></div>
    <div class="gp-field"><label for="${id}-line2">${esc(this.text("line2"))}</label>
      <input id="${id}-line2" name="${p}address-line2" autocomplete="address-line2"></div>
    <div class="gp-field"><label for="${id}-city">${esc(this.text("city"))}</label>
      <input id="${id}-city" name="${p}city" autocomplete="address-level2"></div>
    <div class="gp-field"><label for="${id}-pc">${esc(this.text("postcode"))}</label>
      <input id="${id}-pc" name="${p}postcode" autocomplete="postal-code" spellcheck="false"></div>
  </div>
</fieldset>`;
    const $ = (s) => this.querySelector(`#${id}-${s}`);
    this.el = { postcode: $("postcode"), number: $("number"), go: $("go"), status: $("status"), results: $("results"),
      legend: $("results-legend"), list: $("list"), use: $("use"), manualLink: $("manual-link"), manual: $("manual"),
      line1: $("line1"), city: $("city"), pc: $("pc"), postcodeError: $("postcode-error"), chooseError: $("choose-error") };
    this.found = [];
    this.el.go.addEventListener("click", () => this.find());
    this.el.postcode.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); this.find(); } });
    this.el.number.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); this.find(); } });
    this.el.use.addEventListener("click", () => this.use());
    this.el.manualLink.addEventListener("click", () => this.showManual(true));
  }

  endpoint() {
    return (this.getAttribute("endpoint") || window.location.origin).replace(/\/+$/, "");
  }

  error(which, message) {
    const box = which === "postcode" ? this.el.postcodeError : this.el.chooseError;
    const field = which === "postcode" ? this.el.postcode : null;
    box.textContent = message ? `Error: ${message}` : "";
    box.hidden = !message;
    if (field) {
      if (message) {
        field.setAttribute("aria-invalid", "true");
        field.setAttribute("aria-describedby", box.id);
      } else {
        field.removeAttribute("aria-invalid");
        field.removeAttribute("aria-describedby");
      }
    }
  }

  say(message) {
    this.el.status.textContent = message;
  }

  showManual(focus) {
    this.el.manual.hidden = false;
    if (focus) this.el.line1.focus();
  }

  async find() {
    const postcode = this.el.postcode.value.trim();
    this.error("postcode", "");
    this.error("choose", "");
    if (!postcode) {
      this.error("postcode", this.text("error-postcode"));
      this.el.postcode.focus();
      return;
    }
    const limit = Number(this.getAttribute("limit") || 20);
    const params = new URLSearchParams({ postcode, limit: String(limit) });
    const number = this.el.number.value.trim();
    if (number) params.set("number", number);
    let body;
    try {
      const r = await fetch(`${this.endpoint()}/v1/addresses?${params}`);
      if (!r.ok) throw new Error(String(r.status));
      body = await r.json();
    } catch (e) {
      // the form never depends on the finder: the manual fields are the way on
      this.el.results.hidden = true;
      this.say(this.text("down"));
      this.showManual(false);
      return;
    }
    this.found = body.addresses || [];
    if (this.found.length === 0) {
      this.el.results.hidden = true;
      this.say(this.text("none", { postcode }));
      this.showManual(false);
      return;
    }
    const name = `${this.id_}-choice`;
    this.el.list.innerHTML = this.found.map((a, i) => `
      <div class="gp-radio"><input type="radio" id="${this.id_}-a${i}" name="${name}" value="${i}" form="${this.id_}-none">
      <label for="${this.id_}-a${i}">${esc(a.label)}</label></div>`).join("");
    this.el.results.hidden = false;
    const n = this.found.length;
    const more = body.more ? ` ${this.text(number ? "more-number" : "more", { n })}` : "";
    this.say((n === 1 ? this.text("one") : this.text("found", { n })) + more);
    this.el.legend.focus();
  }

  use() {
    const checked = this.querySelector(`input[name="${this.id_}-choice"]:checked`);
    if (!checked) {
      this.error("choose", this.text("error-choose"));
      return;
    }
    this.error("choose", "");
    const a = this.found[Number(checked.value)];
    const line1 = String(a.label).split(", ")[0];
    this.el.line1.value = line1;
    this.el.city.value = a.commune || "";
    this.el.pc.value = a.postcode || "";
    this.showManual(false);
    this.el.results.hidden = true;
    this.say(`${this.text("use")}: ${a.label}`);
    this.el.line1.focus();
    this.dispatchEvent(new CustomEvent("gridpin-address", { bubbles: true, detail: a }));
  }
}

if (!customElements.get("gridpin-address-finder")) customElements.define("gridpin-address-finder", GridpinAddressFinder);
