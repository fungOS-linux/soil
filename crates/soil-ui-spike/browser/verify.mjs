// Same module, same shim, same bundle as the page -- run headless so the
// browser path has evidence behind it. Fails loudly if the view paints nothing.
import fs from "node:fs";
import path from "node:path";
import url from "node:url";
import { createSpore } from "./run.js";
import { paint } from "./paint.js";

const here = path.dirname(url.fileURLToPath(import.meta.url));

const spore = await createSpore({
  wasm: fs.readFileSync(path.join(here, "spore.wasm")),
  setup: fs.readFileSync(path.join(here, "bundle.lua"), "utf8"),
});

// The state the native trace was taken with.
const commands = spore.frame({
  width: 640,
  height: 360,
  pointer: { x: 120, y: 100 },
  hover: "nav.neo.neo",
  pressed: "",
  focus: "nav.neo.neo",
  selected: "studio",
});
const counts = {};
for (const [kind] of commands) counts[kind] = (counts[kind] ?? 0) + 1;

// The native host emitted these under the same state; a different count means
// the view took a different path, not that the painter differs.
const trace = fs.readFileSync(path.join(here, "..", "parity", "native.trace"), "utf8").trim().split("\n");
console.log(`${commands.length} commands: ${JSON.stringify(counts)}`);
if (commands.length !== trace.length) {
  console.error(`parity/native.trace has ${trace.length} commands; the view emitted ${commands.length}`);
  process.exit(1);
}
console.log(`matches parity/native.trace: ${trace.length} commands`);
console.log(JSON.stringify(commands[0]));

// The page's painter, against a stub context: every command must reach it
// without throwing, or a browser would show a half-painted frame.
const drawn = [];
const stub = new Proxy(
  {},
  {
    get(_target, key) {
      if (key === "measureText") return (text) => ({ width: String(text).length * 6 });
      if (key === "roundRect") return undefined;
      return (...args) => drawn.push([key, ...args]);
    },
    set() {
      return true;
    },
  },
);
const handled = paint(commands, stub, 640, 360);
const fills = drawn.filter(([name]) => name === "fillRect" || name === "fillText").length;
if (handled !== commands.length || fills < trace.length - 5) {
  console.error(`painter handled ${handled} commands and drew ${fills} primitives`);
  process.exit(1);
}
console.log(`painter: ${handled} commands, ${fills} draw calls`);
