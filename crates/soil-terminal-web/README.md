# soil-terminal-web

A shell in the browser, rendered by **`soil-vt`**. The browser is a presenter,
not a second terminal: the server runs a real pty, feeds its bytes to the same
emulator the native terminal window uses, and streams styled rows over
Server-Sent Events. Keystrokes come back as POSTs.

```text
browser ── EventSource ── soil-terminal-web ── pty ── ssh | mush | $SHELL
        └── POST /input ──┘
```

```sh
# local shell
soil-terminal-web

# a remote shell over ssh
soil-terminal-web --ssh aj@home-pi

# mush inside soil-vt (build the host first: cargo build -p mush)
soil-terminal-web --command target/debug/mush \
  --wasm spores/mush/module.wasm --root ~/.local/share/fungos/vfs
```

Open the printed URL. `--listen` and `--token` must come **before**
`--command`, because `--command` consumes the rest of the line.

## Endpoints

| Method | Path | Meaning |
|---|---|---|
| `GET` | `/` | the page |
| `GET` | `/events` | SSE: one `frame` per change, `exit` at the end |
| `POST` | `/input` | raw bytes for the pty (keystrokes) |
| `POST` | `/scroll` | `{"delta":N}`; negative is up into history |
| `POST` | `/paste` | raw bytes; `x-filename` + `content-type`; returns the CID and path |
| `POST` | `/resize` | `{"cols":N,"rows":N}` |
| `GET` | `/health` | `ok` |

A frame is a diff: `{"full":false,"rows":[{"y":3,"runs":[["text",fg,bg,flags]]}]}`.
The first frame after a connect, and any resize, is `full`. Flags are
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

## Tests

```sh
cargo test -p soil-terminal-web
```

The suite covers frame diffing, style runs, request parsing, resize and scroll
clamping, loopback detection, ssh argv construction, the content-addressed
store, and a real pty feeding the emulator.

## Security

This is a remote shell. The listener defaults to `127.0.0.1`; a non-loopback
address **requires** `--token`, and every endpoint checks it.

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
