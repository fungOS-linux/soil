/**
 * mush — turn the agent's shell into Mush for this session, on a chosen Desktop.
 *
 * `/mush`                enable; pick the Desktop this directory belongs to
 * `/mush <desktop>`      enable on a named Desktop
 * `/mush off`            disable
 * `/mush status`         show what is selected
 *
 * While enabled, the `mush` tool runs `mush sh -c`: VFS commands go to the
 * Spore and allowlisted host programs (git, cargo, gh, rg, sed, make) run
 * against an overlay of the selected Desktop. There is no raw host shell, and
 * the host file tools (read/write/edit/grep/find/ls) are swapped for `vfs_*`
 * tools that read and write the same Spore, so paths mean one thing only.
 *
 * Config: MUSH_BIN (default "mush"), MUSH_TIMEOUT (default 900s), MUSH_DESKTOP (Desktop to use
 * when `mush origin` cannot tell which one owns the working directory).
 */
import { spawnSync } from "node:child_process";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

const BIN = process.env.MUSH_BIN || "mush";
const TIMEOUT = process.env.MUSH_TIMEOUT || "900";
/** Desktop to use when `mush origin` cannot tell which one owns the working directory. */
const DEFAULT_DESKTOP = process.env.MUSH_DESKTOP || null;

type Shell = {
        enabled: boolean;
        desktop: string | null;
        root: string | null;
        /** Host shell tools swapped out while mush is on; `/mush off` puts them back. */
        suspended: string[];
};

const shell: Shell = { enabled: false, desktop: null, root: null, suspended: [] };

function textResult(text: string, isError = false) {
        return {
                content: [{ type: "text" as const, text }],
                details: { isError },
        };
}

function mush(args: string[], stdin?: string) {
        const result = spawnSync(BIN, args, {
                encoding: "utf8",
                input: stdin,
                stdio: [stdin === undefined ? "ignore" : "pipe", "pipe", "pipe"],
                maxBuffer: 32 * 1024 * 1024,
        });
        return {
                status: result.status ?? 1,
                out: `${result.stdout || ""}${result.stderr || ""}`,
                error: result.error ? String(result.error) : null,
        };
}

function shellQuote(value: string): string {
        return `'${value.replace(/'/g, "'\\''")}'`;
}

/** Tools that run commands on the host. While mush is on they are swapped out for `mush`. */
const HOST_SHELL_TOOLS = ["bash", "powershell"];

/** Tools that address the host filesystem: the same paths in the VFS are different files. */
const FILE_TOOLS = ["read", "write", "edit", "grep", "find", "ls"];

/** The VFS-backed replacements, registered inactive and activated by `/mush`. */
const VFS_TOOLS = [
        "vfs_read",
        "vfs_write",
        "vfs_edit",
        "vfs_grep",
        "vfs_find",
        "vfs_ls",
        "vfs_diff",
        "vfs_json",
];

/** Everything `/mush` takes away from the model. */
const HOST_TOOLS = [...HOST_SHELL_TOOLS, ...FILE_TOOLS];

/** Make `mush` the agent's only shell: the host shell tools leave the declared tool set. */
function giveShellToMush(pi: ExtensionAPI) {
        const active = pi.getActiveTools();
        const host = active.filter((name) => HOST_TOOLS.includes(name));
        // Re-running `/mush <desktop>` must not forget what `/mush off` has to restore.
        if (host.length) shell.suspended = host;
        pi.setActiveTools([...new Set([...active.filter((name) => !HOST_TOOLS.includes(name)), "mush", ...VFS_TOOLS])]);
}

/**
 * Re-attach to the Desktop after a reload: the extension's own state is gone, but the tool set
 * still has `mush` instead of the host shell. Tool state is only readable once the runtime
 * exists, so this runs on use, never during extension loading.
 */
function tryAutoEnable(pi: ExtensionAPI): boolean {
        if (shell.enabled) return true;
        const active = pi.getActiveTools();
        // `/mush off` already handed the host shell back; do not reclaim it.
        if (active.some((name) => HOST_TOOLS.includes(name))) return false;
        const resolved = originDesktop();
        const desktop = resolved.desktop ?? DEFAULT_DESKTOP;
        const show = desktop ? mush(["desktop", "show", desktop]) : null;
        if (!desktop || !show || show.status !== 0) {
                giveShellBack(pi);
                return false;
        }
        shell.enabled = true;
        shell.desktop = desktop;
        shell.root = /^root:\s*(.+)$/m.exec(show.out)?.[1]?.trim() ?? null;
        giveShellToMush(pi);
        return true;
}

/** Put the host shell back and drop `mush`. Never leaves the session without a shell. */
function giveShellBack(pi: ExtensionAPI) {
        const active = pi.getActiveTools().filter((name) => name !== "mush" && !VFS_TOOLS.includes(name));
        const restored = (shell.suspended.length ? shell.suspended : HOST_TOOLS).filter(
                (name) => !active.includes(name),
        );
        pi.setActiveTools([...active, ...restored]);
        shell.suspended = [];
}

/** The Desktop this directory belongs to, via `mush origin`. */
function originDesktop(): { desktop: string | null; note: string } {
        const result = mush(["origin", "--json", "--cwd", process.cwd()]);
        const line = result.out.trim().split("\n").pop() || "";
        try {
                const parsed = JSON.parse(line) as { desktop?: string | null; readiness?: string };
                if (parsed.desktop) {
                        return { desktop: parsed.desktop, note: `${parsed.desktop} (${parsed.readiness})` };
                }
                return { desktop: null, note: `no Desktop owns ${process.cwd()}` };
        } catch {
                return { desktop: null, note: `origin failed: ${line || result.error || "no output"}` };
        }
}

function desktops(): string[] {
        return mush(["desktop", "list"])
                .out.split("\n")
                .map((line) => line.split("\t")[0]?.trim())
                .filter((id): id is string => Boolean(id));
}

function statusText(): string {
        if (!shell.enabled) return "mush: off";
        const host = shell.suspended.length ? `host tools off (${shell.suspended.join(", ")})` : "host tools ON";
        return `mush: on  desktop=${shell.desktop ?? "(origin)"}  root=${shell.root ?? "(config)"}  ${host}`;
}

// ---- VFS file tools -------------------------------------------------------
//
// While mush is on, the host file tools are swapped for these. They read and write the same
// Spore the shell does, so a path means the same thing in `mush` and in `vfs_read` — and it is
// never the host checkout's path.

/** `mush sh -c` writes a startup banner to stdout before the command runs. */
function stripBanner(output: string): string {
        const lines = output.split("\n");
        while (lines.length && /^(mush · |help for builtins)/.test(lines[0] ?? "")) lines.shift();
        return lines.join("\n");
}

/** `mush: <path>: Error { code: 1, name: "not-found", message: "…" }` → `not-found: …`. */
function vfsError(output: string): string | null {
        const match = /Error \{ code: \d+, name: "([^"]+)", message: "([^"]*)" \}/.exec(output);
        return match ? `${match[1]}: ${match[2]}` : null;
}

type VfsRun = { ok: boolean; out: string; err: string | null };

/** Run one command line in the VFS on the selected Desktop. */
function vfs(command: string): VfsRun {
        const args: string[] = [];
        if (shell.desktop) args.push("--desktop", shell.desktop);
        else if (shell.root) args.push("--root", shell.root);
        args.push("--timeout", TIMEOUT, "--adaptive-background", "sh", "-c", command);
        const result = mush(args);
        const out = stripBanner(result.out);
        const err = result.error ? String(result.error) : vfsError(out);
        return { ok: result.status === 0 && !err, out, err };
}


/**
 * `mush: …` lines are the shell's own refusals (`json get …: no value at …`), never document
 * or diff content: a diff line always starts with a space, `+`, `-`, `\\`, `@`, or a header.
 */
function shellError(output: string): string | null {
        const line = output.split("\n").find((entry) => entry.startsWith("mush: "));
        return line ? line.slice("mush: ".length) : null;
}

const VFS_PATH_NOTE = "Paths are VFS-absolute: the working directory is `/`, never the host checkout.";

export default function (pi: ExtensionAPI) {
        pi.registerCommand("mush", {
                description: "Switch this session's shell to Mush, optionally on a named Desktop",
                handler: async (args, ctx) => {
                        const [first, ...rest] = args.trim().split(/\s+/).filter(Boolean);
                        try {
                                if (first === "off") {
                                        shell.enabled = false;
                                        shell.desktop = null;
                                        shell.root = null;
                                        giveShellBack(pi);
                                        ctx.ui.notify("mush: off  (host tools restored)", "info");
                                        return;
                                }
                                if (first === "status" || (!first && shell.enabled)) {
                                        ctx.ui.notify(statusText(), "info");
                                        return;
                                }

                                let desktop = first && first !== "on" ? first : null;
                                let note = "";
                                if (!desktop) {
                                        const origin = originDesktop();
                                        desktop = origin.desktop ?? DEFAULT_DESKTOP;
                                        note = origin.desktop
                                                ? ` (${origin.note})`
                                                : ` (MUSH_DESKTOP; ${origin.note})`;
                                }
                                if (!desktop) {
                                        const known = desktops();
                                        ctx.ui.notify(
                                                `mush: no Desktop selected${note}\nusage: /mush <desktop>${known.length ? `\nknown: ${known.join(", ")}` : ""}`,
                                                "error",
                                        );
                                        return;
                                }

                                const show = mush(["desktop", "show", desktop]);
                                if (show.status !== 0) {
                                        ctx.ui.notify(`mush: unknown Desktop ${desktop}\n${show.out.trim()}`, "error");
                                        return;
                                }
                                const root = /^root:\s*(.+)$/m.exec(show.out)?.[1]?.trim() ?? null;
                                shell.enabled = true;
                                shell.desktop = desktop;
                                shell.root = root;
                                giveShellToMush(pi);
                                const listing = mush(["projects", "--desktop", desktop, "--json"]);
                                let projects = "";
                                try {
                                        const parsed = JSON.parse(listing.out.trim()) as {
                                                projects?: Array<{ name: string; present: boolean }>;
                                        };
                                        const names = (parsed.projects || [])
                                                .filter((project) => project.present)
                                                .map((project) => project.name);
                                        if (names.length) projects = `\nprojects: ${names.join(", ")}`;
                                } catch {
                                        /* the list is a convenience; enabling still succeeded */
                                }
                                const host = shell.suspended.length ? `host tools off (${shell.suspended.join(", ")})` : "host tools ON";
                                ctx.ui.notify(
                                        `mush: on  desktop=${desktop}${note}${root ? `  root=${root}` : ""}  ${host}${projects}`,
                                        "info",
                                );
                                if (rest.length) ctx.ui.notify("mush: ignoring extra arguments", "info");
                        } catch (error) {
                                ctx.ui.notify(`mush: ${String(error)}`, "error");
                        }
                },
        });

        pi.registerTool({
                name: "mush",
                label: "mush shell",
                defaultActive: false,
                description:
                        "Run a shell command inside Mush on the selected Desktop. VFS commands run in the Spore; " +
                        "allowlisted host programs (git, cargo, gh, rg, sed, make) run against an overlay of the VFS. " +
                        "No raw host shell, no network except through an allowlisted program.",
                parameters: Type.Object({
                        command: Type.String({ description: "Command line. Chained with ;, && and ||." }),
                        cwd: Type.Optional(
                                Type.String({ description: "VFS directory to run in. Mush does not persist cd between calls." }),
                        ),
                }),
                async execute(_id, params) {
                        if (!tryAutoEnable(pi)) {
                                return textResult("mush is off for this session; ask the human to run /mush first", true);
                        }
                        const { command, cwd } = params as { command: string; cwd?: string };
                        const line = cwd ? `cd ${shellQuote(cwd)} && ${command}` : command;
                        const args: string[] = [];
                        if (shell.desktop) args.push("--desktop", shell.desktop);
                        else if (shell.root) args.push("--root", shell.root);
                        args.push("--timeout", TIMEOUT, "--adaptive-background", "sh", "-c", line);
                        const result = mush(args);
                        if (result.error) {
                                return textResult(`mush failed to start: ${result.error}`, true);
                        }
                        const output = result.out;
                        const trimmed =
                                output.length > 200_000 ? `${output.slice(0, 200_000)}\n[mush: output truncated]\n` : output;
                        return textResult(trimmed || `[exit ${result.status}]`, result.status !== 0);
                },
        });

        pi.registerTool({
                name: "vfs_read",
                label: "vfs read",
                defaultActive: false,
                description: `Read a file from the Mush VFS. ${VFS_PATH_NOTE} Returns the file's lines, numbered.`,
                parameters: Type.Object({
                        path: Type.String({ description: "VFS-absolute path, e.g. /project/src/main.rs" }),
                        offset: Type.Optional(Type.Number({ description: "1-indexed first line to return" })),
                        limit: Type.Optional(Type.Number({ description: "How many lines to return" })),
                }),
                async execute(_id, params) {
                        if (!tryAutoEnable(pi)) return textResult("mush is off for this session", true);
                        const { path, offset, limit } = params as { path: string; offset?: number; limit?: number };
                        const call = vfs(`cat ${shellQuote(path)}`);
                        if (!call.ok) return textResult(`read ${path} failed: ${call.err ?? "unknown error"}`, true);
                        const lines = call.out.replace(/\n$/, "").split("\n");
                        const start = offset && offset > 1 ? offset - 1 : 0;
                        const end = limit ? start + limit : lines.length;
                        const body = lines
                                .slice(start, end)
                                .map((line, index) => `${start + index + 1}\t${line}`)
                                .join("\n");
                        return textResult(`${path} (${lines.length} ${lines.length === 1 ? "line" : "lines"})\n${body}`, false);
                },
        });

        pi.registerTool({
                name: "vfs_write",
                label: "vfs write",
                defaultActive: false,
                description: `Create or replace a file in the Mush VFS. ${VFS_PATH_NOTE} Content is written byte for byte: no trailing newline is added or removed.`,
                parameters: Type.Object({
                        path: Type.String({ description: "VFS-absolute path" }),
                        content: Type.String({ description: "Full file contents" }),
                }),
                async execute(_id, params) {
                        if (!tryAutoEnable(pi)) return textResult("mush is off for this session", true);
                        const { path, content } = params as { path: string; content: string };
                        // `write` normalises a trailing newline onto the file; `printf '%s'` does not,
                        // so edits round-trip byte for byte.
                        const call = vfs(`printf '%s' ${shellQuote(content)} > ${shellQuote(path)}`);
                        if (!call.ok) return textResult(`write ${path} failed: ${call.err ?? "unknown error"}`, true);
                        return textResult(call.out.trim() || `wrote ${path} (${content.length} bytes)`, false);
                },
        });

        pi.registerTool({
                name: "vfs_edit",
                label: "vfs edit",
                defaultActive: false,
                description:
                        `Replace an exact string in a file in the Mush VFS. ${VFS_PATH_NOTE} ` +
                        "`old_string` must appear exactly once unless `replace_all` is set.",
                parameters: Type.Object({
                        path: Type.String({ description: "VFS-absolute path" }),
                        old_string: Type.String({ description: "Exact text to replace" }),
                        new_string: Type.String({ description: "Replacement text" }),
                        replace_all: Type.Optional(Type.Boolean({ description: "Replace every occurrence" })),
                }),
                async execute(_id, params) {
                        if (!tryAutoEnable(pi)) return textResult("mush is off for this session", true);
                        const { path, old_string, new_string, replace_all } = params as {
                                path: string;
                                old_string: string;
                                new_string: string;
                                replace_all?: boolean;
                        };
                        const read = vfs(`cat ${shellQuote(path)}`);
                        if (!read.ok) return textResult(`edit ${path} failed: ${read.err ?? "unknown error"}`, true);
                        // Raw bytes: stripping the trailing newline here is what made an edit
                        // change a file it only meant to touch in one place.
                        const original = read.out;
                        const hits = old_string ? original.split(old_string).length - 1 : 0;
                        if (hits === 0) return textResult(`edit ${path} failed: old_string not found`, true);
                        if (hits > 1 && !replace_all) {
                                return textResult(
                                        `edit ${path} failed: old_string appears ${hits} times; pass replace_all or add context`,
                                        true,
                                );
                        }
                        const at = original.indexOf(old_string);
                        const updated = replace_all
                                ? original.split(old_string).join(new_string)
                                : original.slice(0, at) + new_string + original.slice(at + old_string.length);
                        const write = vfs(`printf '%s' ${shellQuote(updated)} > ${shellQuote(path)}`);
                        if (!write.ok) return textResult(`edit ${path} failed: ${write.err ?? "unknown error"}`, true);
                        return textResult(`${write.out.trim() || `wrote ${path}`}\n${hits} replacement(s) in ${path}`, false);
                },
        });

        pi.registerTool({
                name: "vfs_grep",
                label: "vfs grep",
                defaultActive: false,
                description: `Regex search inside the Mush VFS. ${VFS_PATH_NOTE}`,
                parameters: Type.Object({
                        pattern: Type.String({ description: "Regular expression to search for" }),
                        path: Type.Optional(Type.String({ description: "VFS path to search; defaults to /" })),
                }),
                async execute(_id, params) {
                        if (!tryAutoEnable(pi)) return textResult("mush is off for this session", true);
                        const { pattern, path } = params as { pattern: string; path?: string };
                        const call = vfs(`tgrep ${shellQuote(pattern)} ${shellQuote(path || "/")}`);
                        // `tgrep` reports "no matches" as empty output, not as an error.
                        const out = call.out.trim();
                        if (out) return textResult(out, false);
                        if (call.err) return textResult(`grep failed: ${call.err}`, true);
                        return textResult(`no matches for ${JSON.stringify(pattern)}`, false);
                },
        });

        pi.registerTool({
                name: "vfs_find",
                label: "vfs find",
                defaultActive: false,
                description: `Find files by glob in the Mush VFS. ${VFS_PATH_NOTE} Use ** for any depth, e.g. /**/*.rs`,
                parameters: Type.Object({
                        pattern: Type.String({ description: "Glob matched against the whole VFS path" }),
                        path: Type.Optional(Type.String({ description: "VFS subtree to search; defaults to /" })),
                }),
                async execute(_id, params) {
                        if (!tryAutoEnable(pi)) return textResult("mush is off for this session", true);
                        const { pattern, path } = params as { pattern: string; path?: string };
                        // The Spore's own find: no tree parsing, no reconstructing paths from
                        // indentation, and the same matcher the shell uses.
                        const call = vfs(`find ${shellQuote(path || "/")} -name ${shellQuote(pattern)}`);
                        if (!call.ok) return textResult(`find failed: ${call.err ?? "unknown error"}`, true);
                        const hits = call.out.split("\n").filter((line) => line.trim().length > 0);
                        return textResult(hits.length ? hits.join("\n") : `no paths match ${pattern}`, false);
                },
        });

        pi.registerTool({
                name: "vfs_ls",
                label: "vfs ls",
                defaultActive: false,
                description: `List a directory in the Mush VFS. ${VFS_PATH_NOTE}`,
                parameters: Type.Object({
                        path: Type.Optional(Type.String({ description: "VFS directory; defaults to /" })),
                }),
                async execute(_id, params) {
                        if (!tryAutoEnable(pi)) return textResult("mush is off for this session", true);
                        const { path } = params as { path?: string };
                        const dir = path || "/";
                        const call = vfs(`ls -l ${shellQuote(dir)}`);
                        if (!call.ok) return textResult(`ls ${dir} failed: ${call.err ?? "unknown error"}`, true);
                        const rows = call.out
                                .split("\n")
                                .map((line) => /^([fd])\s+(\d+)\s+(\S*)\s+(.*)$/.exec(line.trim()))
                                .filter((m): m is RegExpExecArray => Boolean(m))
                                .map((m) => (m[1] === "d" ? `${m[4]}/` : `${m[4]}  (${m[2]} B)`));
                        return textResult(rows.length ? `${dir}\n${rows.join("\n")}` : `${dir}: empty`, false);
                },
        });

        pi.registerTool({
                name: "vfs_diff",
                label: "vfs diff",
                defaultActive: false,
                description:
                        `Unified line diff between two files in the Mush VFS. ${VFS_PATH_NOTE} ` +
                        "Files that differ are a result, not a failure. The diff is computed inside the Spore, so it costs no overlay.",
                parameters: Type.Object({
                        left: Type.String({ description: "VFS-absolute path of the original file" }),
                        right: Type.String({ description: "VFS-absolute path of the file to compare with" }),
                        context: Type.Optional(
                                Type.Number({ description: "Lines of context around each change (default 3, 0 for none)" }),
                        ),
                        brief: Type.Optional(Type.Boolean({ description: "Only ask whether the files differ" })),
                }),
                async execute(_id, params) {
                        if (!tryAutoEnable(pi)) return textResult("mush is off for this session", true);
                        const { left, right, context, brief } = params as {
                                left: string;
                                right: string;
                                context?: number;
                                brief?: boolean;
                        };
                        const flags = [
                                brief ? "-q" : null,
                                context !== undefined ? `-U ${Math.max(0, Math.floor(context))}` : null,
                        ].filter((flag): flag is string => Boolean(flag));
                        const head = flags.length ? `${flags.join(" ")} ` : "";
                        const call = vfs(`diff ${head}${shellQuote(left)} ${shellQuote(right)}`);
                        // A VFS error is only "not-found: …", so it needs the file names; the shell's own
                        // refusals already name the command and the path.
                        const failure = call.err
                                ? `diff ${left} ${right} failed: ${call.err}`
                                : shellError(call.out);
                        if (failure) return textResult(failure, true);
                        const out = call.out.replace(/\n$/, "");
                        if (!out.trim()) return textResult(`${left} and ${right} are identical`, false);
                        return textResult(out, false);
                },
        });

        pi.registerTool({
                name: "vfs_json",
                label: "vfs json",
                defaultActive: false,
                description:
                        `Read and edit a JSON document in the Mush VFS. ${VFS_PATH_NOTE} ` +
                        "Paths are dot/bracket: `a.b`, `list[0]`, and `[\"a.b\"]` for a key that contains a dot. " +
                        "Edits keep the document's key order and indentation, and are refused if someone else wrote the file first.",
                parameters: Type.Object({
                        action: Type.Union([
                                Type.Literal("get"),
                                Type.Literal("set"),
                                Type.Literal("del"),
                                Type.Literal("merge"),
                                Type.Literal("pretty"),
                        ]),
                        file: Type.String({ description: "VFS-absolute path of the JSON document" }),
                        path: Type.Optional(
                                Type.String({ description: "Document path for get/set/del, e.g. nested.count or list[0]" }),
                        ),
                        value: Type.Optional(
                                Type.String({
                                        description:
                                                'For set: the new value as JSON text. A string needs its JSON quotes: "\"text\"", not "text".',
                                }),
                        ),
                        sources: Type.Optional(
                                Type.Array(Type.String(), {
                                        description: "For merge: documents merged into file, later ones win",
                                }),
                        ),
                        indent: Type.Optional(
                                Type.Number({ description: "For pretty: spaces per level (0 minifies); default keeps 2" }),
                        ),
                }),
                async execute(_id, params) {
                        if (!tryAutoEnable(pi)) return textResult("mush is off for this session", true);
                        const input = params as {
                                action: "get" | "set" | "del" | "merge" | "pretty";
                                file: string;
                                path?: string;
                                value?: string;
                                sources?: string[];
                                indent?: number;
                        };
                        const file = shellQuote(input.file);
                        let command: string;
                        switch (input.action) {
                                case "get":
                                        command = `json get ${file}${input.path ? ` ${shellQuote(input.path)}` : ""}`;
                                        break;
                                case "set":
                                        if (input.value === undefined) {
                                                return textResult("set needs `value`", true);
                                        }
                                        command = `json set ${file} ${shellQuote(input.path ?? "$")} ${shellQuote(input.value)}`;
                                        break;
                                case "del":
                                        if (!input.path) return textResult("del needs `path`", true);
                                        command = `json del ${file} ${shellQuote(input.path)}`;
                                        break;
                                case "merge":
                                        if (!input.sources?.length) return textResult("merge needs `sources`", true);
                                        command = `json merge ${file} ${input.sources.map(shellQuote).join(" ")}`;
                                        break;
                                case "pretty":
                                        command = `json pretty${
                                                input.indent === undefined ? "" : ` --indent ${Math.max(0, Math.floor(input.indent))}`
                                        } ${file}`;
                                        break;
                        }
                        const call = vfs(command);
                        const failure = call.err
                                ? `json ${input.action} ${input.file} failed: ${call.err}`
                                : shellError(call.out);
                        if (failure) return textResult(failure, true);
                        return textResult(call.out.replace(/\n$/, ""), false);
                },
        });

        // `!cmd` is the human's shell. While mush is on it runs in the same place the agent's
        // commands do, so "the session is on mush" is true for both halves of the session.
        // The agent has to know its paths are VFS paths, not host paths, or every file tool call
        // silently addresses the wrong filesystem.
        pi.on("before_agent_start", (event) => {
                if (!shell.enabled) return undefined;
                const where = shell.desktop ? `Desktop "${shell.desktop}"` : `root ${shell.root ?? "(config)"}`;
                return {
                        systemPrompt:
                                `${event.systemPrompt}\n\n## Mush session\n\n` +
                                `The shell and file tools run inside the Mush ${where}. The working directory is \`/\` ` +
                                "in the VFS, not the host checkout. Every path is VFS-absolute " +
                                "(\`/project/src/main.rs\`), never a host path such as \`/Users/...\`. " +
                                "Host shell tools are unavailable while mush is on.\n",
                };
        });

        pi.on("user_bash", (event) => {
                if (!shell.enabled) return undefined;
                const args: string[] = [];
                if (shell.desktop) args.push("--desktop", shell.desktop);
                else if (shell.root) args.push("--root", shell.root);
                args.push("--timeout", TIMEOUT, "sh", "-c", event.command);
                const result = mush(args);
                if (result.error) {
                        // No mush binary after all: hand the shell back rather than blocking `!`.
                        shell.enabled = false;
                        giveShellBack(pi);
                        return undefined;
                }
                return {
                        result: {
                                output: result.out,
                                exitCode: result.status,
                                cancelled: false,
                                truncated: false,
                        },
                };
        });

        // Belt and braces: tool_search can activate `bash` again behind our back. While mush is on
        // a host-shell call is refused, so "no raw host shell" holds even then.
        pi.on("tool_call", (event) => {
                if (!shell.enabled) return undefined;
                if (!HOST_TOOLS.includes(event.toolName)) return undefined;
                return {
                        block: true,
                        reason: "mush is on for this session: use the `mush` tool, not the host shell or host file tools",
                };
        });

}
