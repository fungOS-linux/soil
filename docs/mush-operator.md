# Operator → Mush over MCP

Mush exposes one MCP tool over loopback HTTP:

```text
mcp__mush__mush_exec
```

It executes a Mush command against the server's fixed VFS root, using the same
Spore, records binding, command root and principal as the host process. It does
not run host binaries.

## Start the server

```sh
mush mcp --foreground --listen 127.0.0.1:0 --mcp-token "$MUSH_MCP_TOKEN"
```

`mush mcp` and `mush --serve` are servers: they own the process until they are
stopped. Mush refuses to start either one when stdin is not a terminal unless
`--foreground` says a supervisor owns the process. That refusal exists because a
server started from an agent tool call looks exactly like a hung tool call: no
output, no exit status, nothing to recover from.

If `--mcp-token` or `MUSH_MCP_TOKEN` is omitted, Mush generates a token and
prints it once on stderr. The URL is printed on stderr:

```text
mush: mcp server http://127.0.0.1:PORT/mcp
```

Set a fixed token for a long-lived Operator config. Treat it as a capability
for the VFS: anyone with the token can run `mush_exec` as this principal.

## Operator config

Operator already supports HTTP MCP servers through `[[mcp_server]]`:

```toml
[[mcp_server]]
name = "mush"
url = "http://127.0.0.1:PORT/mcp"
headers = { Authorization = "Bearer YOUR_TOKEN" }
```

After attaching, the tool appears as:

```text
mcp__mush__mush_exec
```

Example arguments:

```json
{"command": "mb ready --json"}
```

The response includes MCP text content, combining stdout and stderr, plus
`structuredContent` with separate `stdout`, `stderr`, `status`, `exit`,
`state`, and `blocking` fields. `isError` is true when the command returns a
non-zero shell status or did not finish.

`state` is one of:

| `state` | `blocking` | Meaning |
|---|---|---|
| `done` | `false` | The command ran to completion |
| `timeout` | `true` | The command exceeded its deadline and the Spore was interrupted |

A command that runs past the daemon deadline is interrupted at the engine level,
not abandoned: `mush_exec` answers instead of hanging, with `state=timeout`,
`status=124`, and a hint naming `--timeout`.

## Recommended placement

```text
Operator
  -> mcp__mush__mush_exec
       VFS, records, installed wasm commands
  -> existing bash tool
       host checkout, compilers, git, network
```

That keeps the host process boundary explicit. Do not add a host-command
escape hatch to `mush_exec`; if Operator needs `cargo`, `git` or `gh`, that is
the separate sandboxed host-tool path.

## Service-account layout

Run the MCP server under the agent service account, for example
`ajmwagar-agents`, with its own VFS root and records book. The bearer token is
not a substitute for the POSIX boundary: it only authenticates this loopback
MCP endpoint.

See [Mush service accounts](mush-service-accounts.md).
