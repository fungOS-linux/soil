// One spore instance, many frames. The module is a reactor: `lua_spore_run`
// compiles and runs whatever chunk the host writes into its buffer, so the
// setup chunk (loader + view modules) runs once and each frame afterwards is a
// few hundred bytes.
import { createWasi } from "./wasi-shim.js";

const BUFFER_SIZE = 64 * 1024;

export async function createSpore({ wasm, setup }) {
  const module =
    wasm instanceof WebAssembly.Module ? wasm : await WebAssembly.compile(wasm);
  const wasi = createWasi({ args: [] });
  const instance = await WebAssembly.instantiate(module, wasi.imports);
  // A reactor initialises lazily: imports are called from `_initialize` and
  // from every frame, so the shim needs the memory first.
  wasi.setMemory(instance.exports.memory);
  if (instance.exports._initialize) instance.exports._initialize();

  const memory = instance.exports.memory;
  const scratch = instance.exports.lua_spore_buffer();
  const encoder = new TextEncoder();

  function run(chunk) {
    const bytes = encoder.encode(chunk);
    if (bytes.length > BUFFER_SIZE) {
      throw new Error(`chunk is ${bytes.length} bytes, the spore buffer is ${BUFFER_SIZE}`);
    }
    // Memory may have grown since the last call, so take a fresh view.
    new Uint8Array(memory.buffer).set(bytes, scratch);
    wasi.clear();
    const code = instance.exports.lua_spore_run(bytes.length);
    const stdout = wasi.takeStdout();
    const stderr = wasi.takeStderr();
    if (code !== 0) {
      throw new Error(stderr.trim() || `lua_spore_run returned ${code}`);
    }
    return stdout;
  }

  run(setup);

  const quoted = (value) => JSON.stringify(String(value ?? ""));

  return {
    /** Render one frame; the host owns extent and input. */
    frame({ width, height, hover = "", pressed = "", focus = "", selected = "", pointer = { x: 0, y: 0 } }) {
      const stdout = run(
        `print(SOIL.render({ width = ${Math.floor(width)}, height = ${Math.floor(height)}, ` +
          `selected = ${quoted(selected)}, hover = ${quoted(hover)}, pressed = ${quoted(pressed)}, ` +
          `focus = ${quoted(focus)}, pointer = { x = ${pointer.x}, y = ${pointer.y} } }))`,
      );
      const line = stdout.trim().split("\n").filter((l) => l.startsWith("[")).pop();
      if (!line) throw new Error(`the spore printed no commands:\n${stdout}`);
      return JSON.parse(line);
    },
  };
}
