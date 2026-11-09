# soil-terminal-web

A browser terminal with a **target picker**, rendered by **`soil-vt`**. The
browser is a presenter, not a second terminal: the server runs a real pty per
session, feeds its bytes to the same emulator the native terminal window uses,
and streams styled rows over Server-Sent Events. Keystrokes come back as POSTs.

```text
browser ── EventSource ── soil-terminal-web ── pty ── zsh | ssh | mush
        └── POST /input ──┘
```

One server hosts many sessions. A session is a **target** — a local shell, an
ssh host, or a Mush desktop — on its own pty and its own emulator. The page
opens, switches, and closes them like a multiplexer; the server owns the session
table, so a reload reattaches to the session named in the URL.

```sh
# pick from ~/.config/fungos/terminal/targets.toml, or from what is on the
# machine: the login shells, the Host aliases in ~/.ssh/config, and the
# desktops under ~/.config/fungos/desktops/
soil-terminal-web

# one target, no picker
soil-terminal-web --ssh aj@home-pi
soil-terminal-web --command target/debug/mush \
  --wasm spores/mush/module.wasm --root ~/.local/share/fungos/vfs
```

Open the printed URL. `--listen`, `--token`, and `--targets` must come
**before** `--command`, because `--command` consumes the rest of the line.

## Targets

The catalog is `~/.config/fungos/terminal/targets.toml` (override with
`--targets FILE`). Every `[[target]]` becomes a button, grouped by `kind`:

```toml
[mush]
binary = "mush"                                 # or an absolute path

[[target]]
name = "studio"
label = "studio"
kind = "shell"
command = ["zsh", "-l"]                         # default: $SHELL

[[target]]
name = "neo"
label = "neo"
kind = "ssh"
host = "averys-macbook-neo"                     # an ~/.ssh/config alias
args = ["-p", "2222"]                           # optional extra ssh args

[[target]]
name = "acme"
label = "acme"
kind = "mush"
desktop = "acme"                                # or root = "/path/to/vfs"
```

`kind = "ssh"` runs `ssh -tt [args...] host`, so host-key and password prompts
appear in the browser like any other output. `kind = "mush"` runs
`mush --desktop <id>` (or `--root <dir>`): one Mush session per desktop, which
is how a virtual or shared desktop gets its own shell. The Mush Spore is
embedded in the host binary; `wasm = "..."` is a development override, and a
stale module fails its type check.

A target has **at most one live session**: opening it again attaches to the one
that is already running, and a session left behind by an exit is replaced. That
is also what keeps Mush honest — it takes a lock per desktop and a second
instance exits 0 without printing anything, which otherwise looks like a crash.

Without a config file the catalog is discovered: the login shells that exist
(`$SHELL`, zsh, bash, sh), the non-pattern `Host` aliases in `~/.ssh/config`,
and `~/.config/fungos/desktops/*.toml` when a `mush` binary resolves
(`SOIL_MUSH_BIN`, `target/debug/mush`, or `mush` on `PATH`). `acme`, `desk`, and
`personal` above are the desktops already on this machine.

## Mush

A Mush target runs the host's embedded Spore. Two things are per desktop:

**`.mushrc`** is a script at the VFS root, run before the first prompt. It is a
normal Mush script, so it can set `PS1` (the prompt understands `\u`, `\h`,
`\w`, `\W`, `\d`, `\$`), export variables, or print a MOTD:

```sh
echo "export PS1='acme:\w$ '" > /.mushrc
echo 'echo mush · acme desktop' >> /.mushrc
echo 'echo help for builtins · exit to leave' >> /.mushrc
```

`\d` is the desktop id when the host selected one; without a `.mushrc` the
default prompt includes it.

**`clear`** is a builtin (ED2 + cursor home), so it works on every surface.
Ctrl-L clears in the line editor, Ctrl-U kills the line, Ctrl-C interrupts.
Host programs run on the pty with inherited stdio, so a program that turns echo
off — a password prompt — reads the terminal the way it would in any shell.

## Endpoints

| Method | Path | Meaning |
|---|---|---|
| `GET` | `/` | the page |
| `GET` | `/targets` | the catalog |
| `GET` | `/sessions` | open sessions and whether their pty has exited |
| `POST` | `/sessions` | `{"target":"name"}`; opens a session and returns `{id,label,target}` |
| `DELETE` | `/sessions/<id>` | kill the pty and forget the session |
| `GET` | `/events?session=N` | SSE: one `frame` per change, `exit` at the end |
| `POST` | `/input?session=N` | raw bytes for that pty (keystrokes) |
| `POST` | `/scroll?session=N` | `{"delta":N}`; negative is up into history |
| `POST` | `/paste` | raw bytes; `x-filename` + `content-type`; returns the CID and path |
| `POST` | `/resize?session=N` | `{"cols":N,"rows":N}` |
| `GET` | `/health` | `ok` |

Omitting `?session=` uses the first open session, which keeps the old
single-shell callers working. A frame is a diff:
`{"full":false,"rows":[{"y":3,"runs":[["text",fg,bg,flags]]}]}`. The first frame
after a connect, and any resize, is `full`. Flags are
`bold=1, italic=2, underline=4, faint=8, inverse=16`. Each frame also carries
`scroll: {"offset":N,"total":N,"len":N}` (or `null`), which drives the
scrollbar and tells the page whether the viewport is in history.

## Scrollback and paste

**Scrollback.** Wheel over the terminal scrolls history; typing snaps back to
the active screen. The cursor is `null` while the viewport is in history, so it
never floats over a row that is not the program's cursor. `soil-vt` owns the
scrollback (bounded by `--scrollback`); the browser only renders the viewport.

**Paste.** Text goes straight to the pty. Files and images are uploaded to a
content-addressed store and the terminal receives the file's **path**:

```text
paste  →  POST /paste  →  <paste-root>/<sha256>/<name>  →  "<path> " into the pty
```

The same bytes pasted twice reuse the same CID and file, so a screenshot
pasted into an agent costs one write. `--paste-root` defaults to
`~/.local/share/fungos/paste`. Drag and drop uses the same path. Text larger
than 64 KiB is uploaded as `paste.txt` instead of typed in.

The inserted path is a **host** path, which is what a normal shell or agent TUI
can read. Running mush (a VFS shell) needs a VFS-relative paste path; that is a
follow-up.

## term.myc

The Mycelium gateway serves this server at **`https://term.myc/`** through one
`routes.json` entry:

```json
{ "name": "term", "hosts": ["term.myc"], "upstream": "http://127.0.0.1:8792", "priority": 10 }
```

The gateway reloads routes every second; the upstream must be the same
`--listen` address the server prints. The gateway cert's SAN is `*.myc` plus
one name per route, so a browser may warn for a name that is not listed
explicitly; `http://127.0.0.1:8792/` is the no-warning URL.

## Tests

```sh
cargo test -p soil-terminal-web
```

The suite covers frame diffing, style runs, request parsing, resize and scroll
clamping, loopback detection, ssh argv construction, target config parsing and
ssh-config discovery, the content-addressed store, and a real pty feeding the
emulator.

## Security

This is a remote shell. The listener defaults to `127.0.0.1`; a non-loopback
address **requires** `--token`, and every endpoint checks it. A session is a
pty for the server's user; the picker is not an authorization boundary.

```sh
soil-terminal-web --listen 0.0.0.0:8790 --token "$(openssl rand -hex 16)" --ssh aj@home-pi
```

The token travels in the query string because `EventSource` cannot set headers,
so it lands in browser history and any proxy log. The better remote pattern is
an SSH tunnel and a loopback listener:

```sh
ssh -L 8790:127.0.0.1:8790 mac
# then open http://127.0.0.1:8790/
```

Do not expose this to the open internet. The pty inherits the server's user and
`~/.ssh`; `--ssh` uses the normal `known_hosts`, so the host-key prompt is the
one you would see in a terminal.
