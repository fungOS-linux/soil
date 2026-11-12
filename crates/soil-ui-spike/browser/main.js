// Browser host: C Lua 5.4 (wasm32-wasip1) runs the view, this page owns the
// surface -- extent, pointer, hover/press/focus, selection -- and paints the
// commands the view returns. The same division the native host has.
import { createSpore } from "./run.js";
import { paint } from "./paint.js";

/** Logical width the layout is designed at; zoom keeps it readable on big screens. */
const BASE_WIDTH = 1000;

const canvas = document.getElementById("surface");
const status = document.getElementById("status");

const state = {
  hover: "",
  pressed: "",
  focus: "",
  selected: "studio",
  pointer: { x: 0, y: 0 },
};

let spore = null;
let regions = [];
let zoom = 1;
let queued = false;

function say(text) {
  status.textContent = text;
}

function logical(event) {
  const box = canvas.getBoundingClientRect();
  return { x: (event.clientX - box.left) / zoom, y: (event.clientY - box.top) / zoom };
}

/** Topmost region under a point, in paint order -- the host hit test. */
function hitAt(point) {
  for (let i = regions.length - 1; i >= 0; i--) {
    const region = regions[i];
    const [, , x, y, w, h] = region;
    if (point.x >= x && point.x < x + w && point.y >= y && point.y < y + h) return region;
  }
  return null;
}

const idOf = (region) => region[1];

function activate(region) {
  const action = region[6] ?? "";
  say(`action ${idOf(region)} → ${action}`);
  if (action.startsWith("open:")) state.selected = action.slice(5);
}

function schedule() {
  if (queued) return;
  queued = true;
  requestAnimationFrame(draw);
}

function draw() {
  queued = false;
  if (!spore) return;

  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const cssWidth = canvas.clientWidth;
  const cssHeight = canvas.clientHeight;
  if (cssWidth < 2 || cssHeight < 2) return;

  // Host chooses the scale; the view is laid out in logical pixels it is told.
  zoom = Math.min(2, Math.max(1, cssWidth / BASE_WIDTH));
  canvas.width = Math.round(cssWidth * dpr);
  canvas.height = Math.round(cssHeight * dpr);

  const started = performance.now();
  try {
    const commands = spore.frame({
      width: cssWidth / zoom,
      height: cssHeight / zoom,
      pointer: state.pointer,
      hover: state.hover,
      pressed: state.pressed,
      focus: state.focus,
      selected: state.selected,
    });
    regions = commands.filter((command) => command[0] === "hit");

    const ctx = canvas.getContext("2d");
    ctx.setTransform(zoom * dpr, 0, 0, zoom * dpr, 0, 0);
    paint(commands, ctx, cssWidth / zoom, cssHeight / zoom);

    const ms = (performance.now() - started).toFixed(1);
    say(`Lua 5.4 in wasm · ${commands.length} commands · ${ms} ms · ${zoom.toFixed(2)}×`);
  } catch (error) {
    say(String(error));
    console.error(error);
  }
}

canvas.addEventListener("pointermove", (event) => {
  state.pointer = logical(event);
  state.hover = idOf(hitAt(state.pointer) ?? ["hit", ""]) ?? "";
  schedule();
});

canvas.addEventListener("pointerdown", (event) => {
  state.pointer = logical(event);
  const region = hitAt(state.pointer);
  state.pressed = region ? idOf(region) : "";
  if (region) state.focus = idOf(region);
  schedule();
});

canvas.addEventListener("pointerup", (event) => {
  state.pointer = logical(event);
  const region = hitAt(state.pointer);
  if (region && idOf(region) === state.pressed) activate(region);
  state.pressed = "";
  schedule();
});

canvas.addEventListener("pointerleave", () => {
  state.hover = "";
  schedule();
});

window.addEventListener("keydown", (event) => {
  if (event.key === "Tab") {
    event.preventDefault();
    if (regions.length === 0) return;
    const at = regions.findIndex((region) => idOf(region) === state.focus);
    const step = event.shiftKey ? -1 : 1;
    const next = (at + step + regions.length * 2) % regions.length;
    state.focus = idOf(regions[at < 0 ? (step > 0 ? 0 : regions.length - 1) : next]);
    schedule();
  } else if (event.key === "Enter" || event.key === " ") {
    const region = regions.find((r) => idOf(r) === state.focus);
    if (region) {
      event.preventDefault();
      activate(region);
      schedule();
    }
  } else if (event.key === "Escape") {
    state.focus = "";
    schedule();
  }
});

window.addEventListener("resize", schedule);

const [wasmResponse, bundleResponse] = await Promise.all([
  fetch("spore.wasm"),
  fetch("bundle.lua"),
]);
if (!wasmResponse.ok || !bundleResponse.ok) {
  throw new Error("cannot load spore.wasm and bundle.lua — serve this directory over http");
}

spore = await createSpore({
  wasm: await wasmResponse.arrayBuffer(),
  setup: await bundleResponse.text(),
});
schedule();
