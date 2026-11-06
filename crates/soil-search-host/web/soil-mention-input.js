// Themable, composable mention autocomplete.
//
// The element owns only the popup/query behavior. It never knows what a
// candidate means beyond kind/token/label. Hosts can theme it with CSS custom
// properties and ::part(), replace item rendering with `renderCandidate`, or
// cancel `soil-mention-select` to insert the token themselves.

const DEFAULT_LIMIT = 10;
const DEFAULT_DEBOUNCE = 120;

export class SoilMentionInput extends HTMLElement {
  static observedAttributes = ["endpoint", "kinds", "limit", "debounce", "placeholder"];

  constructor() {
    super();
    this.attachShadow({ mode: "open" });
    this._items = [];
    this._active = -1;
    this._timer = 0;
    this._query = null;
    this.renderCandidate = null;
    this.transformRequest = null;
  }

  connectedCallback() {
    if (!this.shadowRoot.querySelector("input")) {
      this.shadowRoot.innerHTML = `
        <style>
          :host {
            --soil-mention-bg: #12171c;
            --soil-mention-fg: #dce5ea;
            --soil-mention-muted: #8395a2;
            --soil-mention-border: #26313b;
            --soil-mention-accent: #7dd3a8;
            --soil-mention-radius: 8px;
            --soil-mention-shadow: 0 12px 32px rgba(0,0,0,.35);
            --soil-mention-font: ui-sans-serif, system-ui, sans-serif;
            display: block;
            font: 14px/1.4 var(--soil-mention-font);
            color: var(--soil-mention-fg);
          }
          .wrap { position: relative; }
          input {
            box-sizing: border-box;
            width: 100%;
            padding: 7px 10px;
            border: 1px solid var(--soil-mention-border);
            border-radius: var(--soil-mention-radius);
            background: var(--soil-mention-bg);
            color: var(--soil-mention-fg);
            font: inherit;
            outline: none;
          }
          input:focus { border-color: var(--soil-mention-accent); }
          .popup {
            position: absolute;
            z-index: 40;
            left: 0;
            right: 0;
            top: calc(100% + 4px);
            max-height: 280px;
            overflow: auto;
            padding: 4px;
            border: 1px solid var(--soil-mention-border);
            border-radius: var(--soil-mention-radius);
            background: var(--soil-mention-bg);
            box-shadow: var(--soil-mention-shadow);
          }
          .popup[hidden] { display: none; }
          .item {
            display: flex;
            gap: 10px;
            align-items: center;
            padding: 7px 8px;
            border-radius: calc(var(--soil-mention-radius) - 2px);
            cursor: pointer;
          }
          .item[aria-selected="true"], .item:hover {
            background: color-mix(in srgb, var(--soil-mention-accent) 14%, transparent);
          }
          .kind {
            flex: none;
            min-width: 54px;
            color: var(--soil-mention-muted);
            font-size: 11px;
            text-transform: uppercase;
            letter-spacing: .05em;
          }
          .body { min-width: 0; }
          .label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
          .subtitle { color: var(--soil-mention-muted); font-size: 12px; }
          .empty { padding: 9px 8px; color: var(--soil-mention-muted); }
        </style>
        <div class="wrap">
          <slot name="prefix"></slot>
          <input part="input" type="text" role="combobox" aria-autocomplete="list" aria-expanded="false" autocomplete="off" spellcheck="false">
          <div part="popup" class="popup" role="listbox" hidden></div>
        </div>`;
      this.input = this.shadowRoot.querySelector("input");
      this.popup = this.shadowRoot.querySelector(".popup");
      this.input.addEventListener("input", () => this._onInput());
      this.input.addEventListener("keydown", (event) => this._onKey(event));
      this.input.addEventListener("blur", () => setTimeout(() => this.close(), 120));
    }
    if (this.hasAttribute("placeholder")) {
      this.input.placeholder = this.getAttribute("placeholder");
    }
  }

  attributeChangedCallback(name, _old, value) {
    if (name === "placeholder" && this.input) this.input.placeholder = value || "";
  }

  get value() { return this.input?.value || ""; }
  set value(next) { if (this.input) this.input.value = next; }

  get endpoint() { return this.getAttribute("endpoint") || "/api/suggest"; }
  get kinds() { return this.getAttribute("kinds") || ""; }
  get limit() { return Number(this.getAttribute("limit") || DEFAULT_LIMIT); }
  get debounce() { return Number(this.getAttribute("debounce") || DEFAULT_DEBOUNCE); }

  focus() { this.input?.focus(); }
  close() {
    if (!this.popup) return;
    this.popup.hidden = true;
    this.input.setAttribute("aria-expanded", "false");
    this._items = [];
    this._active = -1;
  }

  _onInput() {
    const query = this._queryAtCaret();
    this._query = query;
    if (!query || query.text.length < 1) {
      this.close();
      this.dispatchEvent(new CustomEvent("soil-mention-query", { detail: null, bubbles: true }));
      return;
    }
    this.dispatchEvent(new CustomEvent("soil-mention-query", {
      detail: { ...query },
      bubbles: true,
    }));
    clearTimeout(this._timer);
    this._timer = setTimeout(() => this._fetch(query), this.debounce);
  }

  async _fetch(query) {
    const params = new URLSearchParams({ q: query.text, limit: String(this.limit) });
    if (this.kinds) params.set("kinds", this.kinds);
    let request = { url: `${this.endpoint}?${params}`, query };
    if (this.transformRequest) request = this.transformRequest(request) || request;
    try {
      const response = await fetch(request.url, { headers: { accept: "application/json" } });
      if (!response.ok) throw new Error(`suggest ${response.status}`);
      const body = await response.json();
      if (this._query?.text !== query.text) return;
      this._items = Array.isArray(body.candidates) ? body.candidates : [];
      this._active = this._items.length ? 0 : -1;
      this._render();
    } catch {
      if (this._query?.text === query.text) {
        this._items = [];
        this._active = -1;
        this._render();
      }
    }
  }

  _render() {
    if (!this.popup) return;
    this.popup.replaceChildren();
    this.popup.hidden = this._items.length === 0;
    this.input.setAttribute("aria-expanded", String(!this.popup.hidden));
    if (!this._items.length) return;
    this._items.forEach((candidate, index) => {
      const item = document.createElement("div");
      item.className = "item";
      item.setAttribute("part", "item");
      item.setAttribute("role", "option");
      item.setAttribute("aria-selected", String(index === this._active));
      if (this.renderCandidate) {
        this.renderCandidate(candidate, item, this);
      } else {
        const kind = document.createElement("span");
        kind.className = "kind";
        kind.setAttribute("part", "kind");
        kind.textContent = candidate.kind || "";
        const body = document.createElement("div");
        body.className = "body";
        const label = document.createElement("div");
        label.className = "label";
        label.setAttribute("part", "label");
        label.textContent = candidate.label || candidate.token || "";
        body.append(label);
        if (candidate.subtitle) {
          const subtitle = document.createElement("div");
          subtitle.className = "subtitle";
          subtitle.setAttribute("part", "subtitle");
          subtitle.textContent = candidate.subtitle;
          body.append(subtitle);
        }
        item.append(kind, body);
      }
      item.addEventListener("mousedown", (event) => {
        event.preventDefault();
        this._select(index);
      });
      this.popup.append(item);
    });
  }

  _onKey(event) {
    if (this.popup?.hidden) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      this._active = Math.min(this._active + 1, this._items.length - 1);
      this._render();
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      this._active = Math.max(this._active - 1, 0);
      this._render();
    } else if (event.key === "Enter" && this._active >= 0) {
      event.preventDefault();
      this._select(this._active);
    } else if (event.key === "Escape") {
      event.preventDefault();
      this.close();
    }
  }

  _select(index) {
    const candidate = this._items[index];
    const query = this._query;
    if (!candidate || !query) return;
    const event = new CustomEvent("soil-mention-select", {
      detail: {
        candidate,
        token: candidate.token,
        replace: { start: query.start, end: query.end },
      },
      bubbles: true,
      cancelable: true,
    });
    if (!this.dispatchEvent(event)) {
      this.close();
      return;
    }
    if (this.input) {
      const value = this.input.value;
      this.input.value = value.slice(0, query.start) + candidate.token + value.slice(query.end);
      this.input.setSelectionRange(
        query.start + candidate.token.length,
        query.start + candidate.token.length,
      );
    }
    this.close();
  }

  _queryAtCaret() {
    if (!this.input) return null;
    const caret = this.input.selectionStart ?? this.input.value.length;
    const before = this.input.value.slice(0, caret);
    const at = before.lastIndexOf("@");
    if (at < 0) return null;
    if (at > 0 && /[\w]/.test(before[at - 1])) return null;
    const text = before.slice(at + 1);
    if (/\s/.test(text)) return null;
    return { text, start: at, end: caret };
  }
}

if (!customElements.get("soil-mention-input")) {
  customElements.define("soil-mention-input", SoilMentionInput);
}
