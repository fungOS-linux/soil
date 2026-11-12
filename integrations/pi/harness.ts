/**
 * Exercise the Mush extension against a real Mush binary.
 *
 * The extension is the agent's only shell and file layer while `/mush` is on,
 * so a broken tool schema or a mis-quoted command line costs a whole session.
 * This harness loads the extension the way Pi does, stubs the Pi API, points
 * `MUSH_BIN` at a wrapper that answers the Desktop lookups and forwards `sh -c`
 * to the real binary against a scratch root, then calls the tools and checks
 * what came back.
 *
 *   bun integrations/pi/harness.ts
 *
 * Exits 0 when the binary is missing, so a checkout without Mush still tests
 * clean. Requires Bun and a Pi install (for `typebox`).
 */
import { chmodSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const mush = Bun.which(process.env.MUSH_BIN || "mush");
if (!mush) {
        console.log("harness: no mush binary on PATH; skipping");
        process.exit(0);
}

/** Pi ships the extension's runtime modules; find the installed release. */
function typeboxEntry(): string {
        const home = process.env.HOME || "";
        const root = process.env.PI_MANAGED_INSTALL_ROOT || join(home, ".pi/agent/install");
        const version = readFileSync(join(root, "current-version"), "utf8").trim();
        return join(root, "releases", version, "node_modules/typebox/build/index.mjs");
}

const work = mkdtempSync(join(tmpdir(), "mush-harness-"));
const scratch = join(work, "vfs");

/** The extension asks the CLI about Desktops; answer those, run the rest. */
const wrapper = join(work, "mush");
writeFileSync(
        wrapper,
        `#!/bin/sh
SCRATCH='${scratch}'
REAL='${mush}'
case "$1" in
  origin) printf '{"desktop":"harness","readiness":"ready"}\\n'; exit 0 ;;
  desktop) printf 'root: %s\\n' "$SCRATCH"; exit 0 ;;
  projects) printf '{"projects":[]}\\n'; exit 0 ;;
esac
while [ $# -gt 0 ]; do
  case "$1" in
    --desktop|--root) shift; [ $# -gt 0 ] && shift ;;
    *) break ;;
  esac
done
exec "$REAL" --root "$SCRATCH" "$@"
`,
);
chmodSync(wrapper, 0o755);
process.env.MUSH_BIN = wrapper;

/** Load the extension with `typebox` resolved from the Pi install. */
const source = readFileSync(join(import.meta.dir, "mush.ts"), "utf8");
const modulePath = join(work, "mush.ts");
writeFileSync(
        modulePath,
        source.replace('from "typebox"', `from "${pathToFileURL(typeboxEntry()).href}"`),
);

function seed(): void {
        const lines = [
                `write a.txt one`,
                `write b.txt two`,
                `write same.txt one`,
                `write c.json '{"name":"mush","nested":{"count":1},"list":[1,2]}'`,
                `write patch.json '{"nested":{"count":2,"extra":true}}'`,
        ];
        const result = Bun.spawnSync([mush!, "--root", scratch, "sh", "-c", lines.join("; ")]);
        if (result.exitCode !== 0) {
                throw new Error(`seed failed: ${result.stderr.toString()}`);
        }
}

seed();

const tools = new Map<string, any>();
const pi = {
        registerCommand() {},
        registerTool(tool: any) {
                tools.set(tool.name, tool);
        },
        on() {},
        getActiveTools: () => [] as string[],
        setActiveTools() {},
};

const { default: install } = await import(pathToFileURL(modulePath).href);
install(pi as any);

let failures = 0;
function check(name: string, actual: string, expected: string, isError = false): void {
        const want = `${isError ? "[error] " : ""}${expected}`;
        const got = `${isError ? "[error] " : ""}${actual}`;
        if (got !== want) {
                failures += 1;
                console.log(`FAIL ${name}\n  want: ${JSON.stringify(want)}\n  got:  ${JSON.stringify(got)}`);
                return;
        }
        console.log(`ok   ${name}`);
}

async function call(name: string, params: unknown) {
        const tool = tools.get(name);
        if (!tool) throw new Error(`tool ${name} is not registered`);
        const result = await tool.execute("harness", params);
        return {
                text: String(result.content?.[0]?.text ?? ""),
                error: Boolean(result.details?.isError),
        };
}

const expected: string[] = ["mush", "vfs_read", "vfs_write", "vfs_edit", "vfs_grep", "vfs_find", "vfs_ls", "vfs_diff", "vfs_json"];
for (const name of expected) {
        check(`registers ${name}`, tools.has(name) ? "yes" : "no", "yes");
}

let r = await call("vfs_diff", { left: "/a.txt", right: "/b.txt" });
check("diff changed", r.text, "--- /a.txt\n+++ /b.txt\n@@ -1 +1 @@\n-one\n+two", r.error);
r = await call("vfs_diff", { left: "/a.txt", right: "/same.txt" });
check("diff identical", r.text, "/a.txt and /same.txt are identical", r.error);
r = await call("vfs_diff", { left: "/a.txt", right: "/b.txt", brief: true });
check("diff brief", r.text, "Files /a.txt and /b.txt differ", r.error);
r = await call("vfs_diff", { left: "/a.txt", right: "/nope.txt" });
check(
        "diff missing",
        r.text.startsWith("diff /a.txt /nope.txt failed: ") ? "yes" : r.text,
        "yes",
);

r = await call("vfs_json", { action: "get", file: "/c.json", path: "name" });
check("json get string", r.text, "mush", r.error);
r = await call("vfs_json", { action: "get", file: "/c.json", path: "nested.count" });
check("json get nested", r.text, "1", r.error);
r = await call("vfs_json", { action: "set", file: "/c.json", path: "nested.count", value: "7" });
check("json set", r.text, "/c.json: set (v2)", r.error);
r = await call("vfs_json", { action: "get", file: "/c.json", path: "nested" });
check("json set took", r.text, '{"count":7}', r.error);
r = await call("vfs_json", { action: "del", file: "/c.json", path: "list[0]" });
check("json del", r.text, "/c.json: del (v3)", r.error);
r = await call("vfs_json", { action: "merge", file: "/c.json", sources: ["/patch.json"] });
check("json merge", r.text, "/c.json: merge (v4)", r.error);
r = await call("vfs_json", { action: "pretty", file: "/c.json", indent: 0 });
check("json pretty", r.text, '{"name":"mush","nested":{"count":2,"extra":true},"list":[2]}', r.error);
r = await call("vfs_json", { action: "get", file: "/c.json", path: "nope" });
check("json missing path", r.error ? "error" : "no error", "error");
r = await call("vfs_json", { action: "set", file: "/c.json", path: "name", value: "bob" });
check("json unquoted string", r.error ? "error" : "no error", "error");

r = await call("vfs_write", { path: "/nested/new.txt", content: "hello\n" });
check("write creates a file", r.error ? "error" : "ok", "ok");
r = await call("vfs_read", { path: "/nested/new.txt" });
// `vfs_read` drops the trailing newline before counting, hence one line.
check("read returns it", r.text, "/nested/new.txt (1 line)\n1\thello", r.error);
r = await call("vfs_edit", { path: "/nested/new.txt", old_string: "hello", new_string: "goodbye" });
check("edit replaces", r.error ? "error" : "ok", "ok");
r = await call("vfs_read", { path: "/nested/new.txt" });
check("edit landed", r.text, "/nested/new.txt (1 line)\n1\tgoodbye", r.error);
r = await call("vfs_find", { pattern: "**/*.txt", path: "/" });
check("find", r.text.includes("/a.txt") && r.text.includes("/nested/new.txt") ? "found" : r.text, "found", r.error);

if (failures) {
        console.log(`\n${failures} check(s) failed`);
        process.exit(1);
}
console.log("\nall checks passed");
