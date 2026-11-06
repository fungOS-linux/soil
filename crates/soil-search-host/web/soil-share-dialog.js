// Reusable share dialog web component.
//
// Presentation only: it knows resource/target tokens and emits intents. The host
// owns ACL, folder policy, Mycelium/Unibus grants, projection rules and audit.
//
// The target candidate contract is the same one used by soil-mention-input:
//   { kind, token, label, subtitle?, visibility? }

const DEFAULT_DEBOUNCE = 120;

export class SoilShareDialog extends HTMLElement {
  static observedAttributes = ["targets-endpoint", "roles", "projections", "title"];

  constructor() {
    super();
    this.attachShadow({ mode: "open" });
    this._targets = [];
    this._selected = new Map();
    this._active = -1;
    this._timer = 0;
    this._resource = null;
    this._shares = [];
    this.renderTarget = null;
    this.transformTargetRequest = null;
  }

  connectedCallback() {
    if (!this.shadowRoot.querySelector(".panel")) this._renderShell();
  }

  get targetsEndpoint() { return this.getAttribute("targets-endpoint") || "/api/share-targets"; }
  get roles() { return (this.getAttribute("roles") || "viewer,commenter,editor").split(",").filter(Boolean); }
  get projections() { return (this.getAttribute("projections") || "default").split(",").filter(Boolean); }

  /** Open for { kind, token, label?, projection? }. */
  show(resource) {
    this._resource = resource || null;
    this._selected.clear();
    this._shares = [];
    this._renderSelected();
    this._renderShares();
    const root = this.shadowRoot.querySelector("dialog");
    if (root && !root.open) root.showModal();
    this.shadowRoot.querySelector("input")?.focus();
    return this;
  }

  close() {
    const root = this.shadowRoot.querySelector("dialog");
    if (root?.open) root.close();
  }

  set shares(items) {
    this._shares = Array.isArray(items) ? items : [];
    this._renderShares();
  }

  get shares() { return this._shares; }

  _renderShell() {
    this.shadowRoot.innerHTML = `
      <style>
        :host {
          --soil-share-bg: #12171c;
          --soil-share-fg: #dce5ea;
          --soil-share-muted: #8395a2;
          --soil-share-border: #26313b;
          --soil-share-accent: #7dd3a8;
          --soil-share-radius: 8px;
          --soil-share-shadow: 0 12px 32px rgba(0,0,0,.35);
          --soil-share-font: ui-sans-serif, system-ui, sans-serif;
          font: 14px/1.4 var(--soil-share-font);
          color: var(--soil-share-fg);
        }
        dialog {
          width: min(560px, calc(100vw - 32px));
          padding: 0;
          border: 1px solid var(--soil-share-border);
          border-radius: var(--soil-share-radius);
          background: var(--soil-share-bg);
          color: var(--soil-share-fg);
          box-shadow: var(--soil-share-shadow);
        }
        dialog::backdrop { background: rgba(0,0,0,.5); }
        .panel { padding: 18px; }
        header { display:flex; align-items:center; gap:10px; margin-bottom:14px; }
        h2 { margin:0; font-size:18px; font-weight:600; flex:1; }
        .resource { color: var(--soil-share-muted); font-size:12px; margin-bottom:12px; }
        .row { display:flex; gap:8px; }
        input, select, button {
          box-sizing:border-box; border:1px solid var(--soil-share-border);
          border-radius: calc(var(--soil-share-radius) - 1px);
          background: var(--soil-share-bg); color: var(--soil-share-fg);
          font: inherit; padding:7px 9px;
        }
        input { flex:1; min-width:0; }
        input:focus, select:focus { border-color: var(--soil-share-accent); outline:none; }
        button { cursor:pointer; }
        button.primary { border-color: var(--soil-share-accent); color: var(--soil-share-accent); }
        .popup { position:relative; }
        .targets { margin:6px 0 0; padding:4px; border:1px solid var(--soil-share-border);
          border-radius: var(--soil-share-radius); max-height:220px; overflow:auto; }
        .targets[hidden] { display:none; }
        .target { display:flex; gap:10px; align-items:center; padding:7px 8px; border-radius:6px; cursor:pointer; }
        .target:hover, .target[aria-selected="true"] { background: color-mix(in srgb, var(--soil-share-accent) 14%, transparent); }
        .kind { flex:none; min-width:54px; color:var(--soil-share-muted); font-size:11px; text-transform:uppercase; letter-spacing:.05em; }
        .label { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
        .subtitle { color:var(--soil-share-muted); font-size:12px; }
        .chips { display:flex; flex-wrap:wrap; gap:6px; margin:10px 0; }
        .chip { display:inline-flex; align-items:center; gap:6px; padding:4px 8px;
          border:1px solid var(--soil-share-border); border-radius:999px; font-size:12px; }
        .chip button { border:0; padding:0; background:none; color:var(--soil-share-muted); }
        .options { display:flex; gap:8px; margin:12px 0; flex-wrap:wrap; }
        .options label { display:flex; align-items:center; gap:6px; color:var(--soil-share-muted); font-size:12px; }
        .existing { margin-top:14px; border-top:1px solid var(--soil-share-border); padding-top:12px; }
        .existing h3 { margin:0 0 8px; font-size:11px; color:var(--soil-share-muted); text-transform:uppercase; letter-spacing:.06em; }
        .share-row { display:flex; gap:8px; align-items:center; padding:6px 0; }
        .share-row .who { flex:1; min-width:0; }
        .share-row .role { color:var(--soil-share-muted); font-size:12px; }
        footer { display:flex; justify-content:flex-end; gap:8px; margin-top:16px; }
      </style>
      <dialog>
        <form method="dialog" class="panel">
          <header><h2 part="title">Share</h2><button value="cancel" aria-label="Close">×</button></header>
          <div class="resource" part="resource"></div>
          <div class="popup">
            <div class="row">
              <input part="input" type="text" role="combobox" aria-autocomplete="list" aria-expanded="false"
                     placeholder="Team, folder, or person" autocomplete="off" spellcheck="false">
            </div>
            <div class="targets" part="targets" role="listbox" hidden></div>
          </div>
          <div class="chips" part="chips"></div>
          <div class="options">
            <label>Role <select class="role"></select></label>
            <label>Projection <select class="projection"></select></label>
          </div>
          <div class="existing">
            <h3>Shared with</h3>
            <div class="shares"></div>
          </div>
          <footer>
            <button value="cancel">Cancel</button>
            <button class="primary" type="button" data-share>Share</button>
          </footer>
        </form>
      </dialog>`;
    const role = this.shadowRoot.querySelector(".role");
    for (const value of this.roles) role.append(new Option(value, value));
    const projection = this.shadowRoot.querySelector(".projection");
    for (const value of this.projections) projection.append(new Option(value, value));
    this._input = this.shadowRoot.querySelector("input");
    this._popup = this.shadowRoot.querySelector(".targets");
    this._input.addEventListener("input", () => this._onInput());
    this._input.addEventListener("keydown", (event) => this._onKey(event));
    this.shadowRoot.querySelector("[data-share]").addEventListener("click", () => this._submit());
  }

  _onInput() {
    const query = this._input.value.trim();
    if (!query) return this._closePopup();
    clearTimeout(this._timer);
    this._timer = setTimeout(() => this._fetchTargets(query), DEFAULT_DEBOUNCE);
  }

  async _fetchTargets(query) {
    const params = new URLSearchParams({ q: query, limit: "10" });
    let request = { url: `${this.targetsEndpoint}?${params}`, query };
    if (this.transformTargetRequest) request = this.transformTargetRequest(request) || request;
    try {
      const response = await fetch(request.url, { headers: { accept: "application/json" } });
      if (!response.ok) throw new Error(`share targets ${response.status}`);
      const body = await response.json();
      if (this._input.value.trim() !== query) return;
      this._targets = Array.isArray(body.targets) ? body.targets : (Array.isArray(body.candidates) ? body.candidates : []);
      this._active = this._targets.length ? 0 : -1;
      this._renderTargets();
    } catch {
      this._targets = [];
      this._active = -1;
      this._renderTargets();
    }
  }

  _renderTargets() {
    const popup = this._popup;
    popup.replaceChildren();
    popup.hidden = this._targets.length === 0;
    this._input.setAttribute("aria-expanded", String(!popup.hidden));
    this._targets.forEach((target, index) => {
      const item = document.createElement("div");
      item.className = "target";
      item.setAttribute("part", "target");
      item.setAttribute("role", "option");
      item.setAttribute("aria-selected", String(index === this._active));
      if (this.renderTarget) {
        this.renderTarget(target, item, this);
      } else {
        const kind = document.createElement("span");
        kind.className = "kind";
        kind.setAttribute("part", "target-kind");
        kind.textContent = target.kind || "";
        const body = document.createElement("div");
        const label = document.createElement("div");
        label.className = "label";
        label.textContent = target.label || target.token || "";
        body.append(label);
        if (target.subtitle) {
          const subtitle = document.createElement("div");
          subtitle.className = "subtitle";
          subtitle.textContent = target.subtitle;
          body.append(subtitle);
        }
        item.append(kind, body);
      }
      item.addEventListener("mousedown", (event) => {
        event.preventDefault();
        this._selectTarget(index);
      });
      popup.append(item);
    });
  }

  _onKey(event) {
    if (this._popup?.hidden) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      this._active = Math.min(this._active + 1, this._targets.length - 1);
      this._renderTargets();
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      this._active = Math.max(this._active - 1, 0);
      this._renderTargets();
    } else if (event.key === "Enter" && this._active >= 0) {
      event.preventDefault();
      this._selectTarget(this._active);
    } else if (event.key === "Escape") {
      event.preventDefault();
      this._closePopup();
    }
  }

  _selectTarget(index) {
    const target = this._targets[index];
    if (!target) return;
    this._selected.set(target.token, target);
    this._renderSelected();
    this._input.value = "";
    this._closePopup();
  }

  _renderSelected() {
    const chips = this.shadowRoot.querySelector(".chips");
    chips.replaceChildren();
    for (const target of this._selected.values()) {
      const chip = document.createElement("span");
      chip.className = "chip";
      chip.setAttribute("part", "chip");
      chip.textContent = target.label || target.token;
      const remove = document.createElement("button");
      remove.type = "button";
      remove.textContent = "×";
      remove.setAttribute("aria-label", `Remove ${target.label || target.token}`);
      remove.addEventListener("click", () => {
        this._selected.delete(target.token);
        this._renderSelected();
      });
      chip.append(remove);
      chips.append(chip);
    }
  }

  _renderShares() {
    const row = this.shadowRoot.querySelector(".shares");
    if (!row) return;
    row.replaceChildren();
    if (!this._shares.length) {
      const empty = document.createElement("div");
      empty.className = "role";
      empty.textContent = "Not shared yet.";
      row.append(empty);
      return;
    }
    for (const share of this._shares) {
      const item = document.createElement("div");
      item.className = "share-row";
      const who = document.createElement("div");
      who.className = "who";
      who.textContent = share.label || share.token || "";
      const role = document.createElement("span");
      role.className = "role";
      role.textContent = share.role || "";
      const revoke = document.createElement("button");
      revoke.type = "button";
      revoke.textContent = "Remove";
      revoke.addEventListener("click", () => {
        const event = new CustomEvent("soil-share-revoke", {
          detail: { resource: this._resource, share },
          bubbles: true,
          cancelable: true,
        });
        this.dispatchEvent(event);
      });
      item.append(who, role, revoke);
      row.append(item);
    }
  }

  _submit() {
    const targets = [...this._selected.values()];
    if (!targets.length) return;
    const event = new CustomEvent("soil-share-request", {
      detail: {
        resource: this._resource,
        targets,
        role: this.shadowRoot.querySelector(".role").value,
        projection: this.shadowRoot.querySelector(".projection").value,
      },
      bubbles: true,
      cancelable: true,
    });
    if (this.dispatchEvent(event)) this.close();
  }

  _closePopup() {
    this._popup.hidden = true;
    this._input.setAttribute("aria-expanded", "false");
    this._targets = [];
    this._active = -1;
  }
}

if (!customElements.get("soil-share-dialog")) {
  customElements.define("soil-share-dialog", SoilShareDialog);
}
