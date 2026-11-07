# Mush service accounts

Status: **prototype**. The per-UID socket and VFS root exist; the signed
principal is carried as a session label, not yet verified by a Mycelium token.

## Shape

Keep the human and agent workloads in different POSIX accounts:

```text
ajmwagar          human shell, personal home, personal credentials
ajmwagar-agents   service account for agent workloads
```

The service account is the OS boundary. It:

- owns its own `$HOME`, VFS root, command root and records binding;
- has no sudo, SSH agent, browser profile or personal keychain access;
- runs `mush --serve` on a `0600` Unix socket under its own home;
- is not readable by the human account except through an explicit grant.

Start with one shared agent service account. Multiple agents can share the UID
and still carry distinct signed principals:

```text
POSIX UID        ajmwagar-agents
soil principal   mycelium:agent:ajmwagar/codex-1
owner            ajmwagar
```

That gives accountability through the principal while keeping the simpler
service-account layout. Do not describe the VFS view as hostile-agent
isolation while agents share a UID: a same-UID process can reach the socket and
root. Move untrusted agents to separate POSIX accounts or a real sandbox.

## Config

As the service account:

```toml
# ~/.config/fungos/mush/config.toml
root = "~/.local/share/fungos/vfs"
command_root = "~/.local/share/fungos/commands"
records_root = "~/.local/share/fungos/records"
records_book = "personal/ajmwagar-agents/marbles"
records_collection = "marbles"
principal = "mycelium:agent:ajmwagar/codex-1"
socket = "~/.local/share/fungos/mush.sock"
```

Check it:

```sh
mush doctor
```

Start the service:

```sh
mush --serve --config ~/.config/fungos/mush/config.toml
```

The service accepts one newline-delimited JSON request per connection:

```json
{"command":"mb --project elmos ready --json"}
```

Response:

```json
{"ok":true,"output":"...","exit":false}
```

The socket is created `0600`; an existing socket is never replaced. The service
checks the accepted peer UID against its own effective UID. That proves the
request came from the service account, not which agent sent it. Attach the
signed Mycelium/Soil principal at the next layer.

## Platform notes

- Linux: a normal local account plus a systemd user unit works well. Keep the
  account's home and socket directory non-world-readable.
- macOS: create a dedicated local account and a per-user `launchd` agent. Do
  not run the service as the human account just because the UI is logged in.
- Do not run one root daemon that switches between principals. The service
  account owns one namespace and one kernel identity.

## Current limits

- `mush --serve` is a service endpoint, not yet a full remote-shell client.
- Principal ownership is a label in this slice; Mycelium token verification is
  the next boundary.
- FUSE is not required for this mode. The service uses the VFS host directly.
