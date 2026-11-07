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
mush mcp --listen 127.0.0.1:0 --mcp-token "$MUSH_MCP_TOKEN"
```

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

The response is MCP text content. `isError` is true when the command returns a
non-zero shell status.

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
