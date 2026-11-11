// The whole of `wasi_snapshot_preview1` that lua.wasm asks for. A browser
// supplies none of it, so every import is answered here: stdout/stderr are
// captured, the clock is real, and the filesystem is refused on purpose -- a
// spore brings its data, it does not open paths.

const EBADF = 8;
const ENOSYS = 52;

/** Thrown by proc_exit, so a caller can tell a clean stop from a crash. */
export class Exit extends Error {
  constructor(code) {
    super(`lua exited with ${code}`);
    this.code = code;
  }
}

export function createWasi({ args = [], onWrite = () => {} } = {}) {
  let memory = null;
  let stdout = "";
  let stderr = "";

  const bytes = () => new Uint8Array(memory.buffer);
  const view = () => new DataView(memory.buffer);
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  const put = (ptr, data) => bytes().set(data, ptr);

  function argsSizesGet(argcPtr, sizePtr) {
    const encoded = args.map((arg) => encoder.encode(`${arg}\0`));
    view().setUint32(argcPtr, args.length, true);
    view().setUint32(sizePtr, encoded.reduce((n, b) => n + b.length, 0), true);
    return 0;
  }

  function argsGet(argvPtr, bufferPtr) {
    let cursor = bufferPtr;
    args.forEach((arg, i) => {
      const encoded = encoder.encode(`${arg}\0`);
      put(cursor, encoded);
      view().setUint32(argvPtr + i * 4, cursor, true);
      cursor += encoded.length;
    });
    return 0;
  }

  function fdWrite(fd, iovs, iovsLen, writtenPtr) {
    const chunks = [];
    let total = 0;
    for (let i = 0; i < iovsLen; i++) {
      const base = view().getUint32(iovs + i * 8, true);
      const len = view().getUint32(iovs + i * 8 + 4, true);
      chunks.push(bytes().slice(base, base + len));
      total += len;
    }
    const joined = new Uint8Array(total);
    let at = 0;
    for (const chunk of chunks) {
      joined.set(chunk, at);
      at += chunk.length;
    }
    const text = decoder.decode(joined);
    if (fd === 2) stderr += text;
    else stdout += text;
    onWrite(fd, text);
    view().setUint32(writtenPtr, total, true);
    return 0;
  }

  const imports = {
    wasi_snapshot_preview1: {
      args_get: argsGet,
      args_sizes_get: argsSizesGet,
      environ_get: () => 0,
      environ_sizes_get: (countPtr, sizePtr) => {
        view().setUint32(countPtr, 0, true);
        view().setUint32(sizePtr, 0, true);
        return 0;
      },
      clock_time_get: (_id, _precision, outPtr) => {
        view().setBigUint64(outPtr, BigInt(Math.round(performance.now() * 1e6)), true);
        return 0;
      },
      fd_write: fdWrite,
      fd_read: (_fd, _iovs, _iovsLen, readPtr) => {
        view().setUint32(readPtr, 0, true);
        return 0;
      },
      fd_close: () => 0,
      fd_renumber: () => 0,
      fd_seek: (_fd, _offset, _whence, newOffsetPtr) => {
        view().setBigUint64(newOffsetPtr, 0n, true);
        return 0;
      },
      fd_fdstat_get: (_fd, ptr) => {
        bytes().fill(0, ptr, ptr + 24);
        bytes()[ptr] = 2; // character device, which is all stdio needs to be
        return 0;
      },
      fd_fdstat_set_flags: () => 0,
      // No preopens: Lua has no directory to look in, so it cannot open a file.
      fd_prestat_get: () => EBADF,
      fd_prestat_dir_name: () => EBADF,
      path_open: () => ENOSYS,
      path_remove_directory: () => ENOSYS,
      path_rename: () => ENOSYS,
      path_unlink_file: () => ENOSYS,
      proc_exit: (code) => {
        throw new Exit(code);
      },
    },
  };

  return {
    imports,
    setMemory(m) {
      memory = m;
    },
    clear() {
      stdout = "";
      stderr = "";
    },
    get stdout() {
      return stdout;
    },
    get stderr() {
      return stderr;
    },
    takeStdout() {
      return stdout;
    },
    takeStderr() {
      return stderr;
    },
    /** Run a command module (`_start`) and return its exit code. */
    run(instance) {
      memory = instance.exports.memory;
      try {
        instance.exports._start();
      } catch (error) {
        if (error instanceof Exit) return error.code;
        throw error;
      }
      return 0;
    },
  };
}
