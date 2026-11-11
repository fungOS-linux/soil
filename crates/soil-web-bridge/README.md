# soil-web-bridge

A browser is a host without Soil. This serves one Spore's Soil capabilities to a
page over HTTP and one WebSocket, so the page can be a skin for a Spore running on
a node.

```sh
cargo run -p soil-web-bridge -- \
  --spore ../spores/spores/worklog \
  --records-root ~/.local/share/fungos/records \
  --book worklog --collection worklog.event
```

Open the printed URL. The page lists the collection, upserts records, deletes
them, and shows a second tab's writes arriving live.

## What this is, and what it is not

It is **an adapter, not an ABI**. Every call names an interface and a function
from the canonical WIT in [`wit/`](../soil-abi). There is no `/records/scan`
route: there is `POST /v1/call` with `{"interface":"soil:records","function":"scan"}`.

It is **a host, not a grant**. The Spore's `[grants]` table decides what the
bridge will do; nothing on the command line widens it. `soil:records.read` gets
`get` and `scan`; `soil:records.write` gets `upsert` and `delete`; a bare
`soil:records` gets everything in the package.

It is **not** the whole of WIT, and it is deliberately not `soil:scene`. Slice one
serves `soil:records` because it exercises the awkward parts — paging, a
revision, an error union, a body that is bytes — with a store that already
exists. The next interfaces (`soil:vfs`, `soil:http`) are a module each; the
protocol does not change.

## The wire

Text frames, JSON, one version.

```jsonc
// request — POST /v1/call, or a text frame on the session
{"v":1,"call":{"id":"7","interface":"soil:records","function":"scan",
                "params":{"collection":"worklog.event","limit":50}}}

// result
{"v":1,"id":"7","result":{"ok":{"entries":[],"next":null}}}
{"v":1,"id":"7","result":{"err":{"code":"denied","message":"..."}}}

// event — server to every session
{"v":1,"event":{"kind":"ready","spore":"dev.fungos.worklog",
                  "grants":["soil:records.read"],
                  "interfaces":{"soil:records":{"book":"worklog","collection":"worklog.event"}}}}
{"v":1,"event":{"kind":"changed","collection":"worklog.event","id":"a","revision":9}}
```

Three rules that are easy to get wrong later:

1. **A WIT error is a value, not a status.** `denied`, `not-found`, `invalid` and
   friends arrive as HTTP 200 with `result.err`, because they are part of the
   interface. HTTP codes are transport only: 400 malformed, 401 unauthorized,
   411 no length, 413 too large, 426 no upgrade, 503 too many sessions.
2. **Field names are camelCase on the wire**, mechanically converted from the
   kebab-case WIT (`updated-at-ms` → `updatedAtMs`). A `resource` becomes an id,
   because a handle cannot cross a socket and pretending otherwise would make the
   bridge a second ABI.
3. **`changed` is a notification, not a delivery.** A slow or sleeping client may
   miss one; it re-scans on reconnect and is correct. That is why the bus drops
   for a full channel instead of blocking a write.

## Sessions and calls

- `POST /v1/call` — one request, one response. The server closes the connection.
- `GET /v1/session` — upgrade to WebSocket. The server sends `ready`, then
  answers calls and pushes events on the same socket.
- `GET /` — the demo page. `GET /healthz` — liveness.

A session is bound to one Spore and its grant set for its whole life. There is no
call that changes what a connection may do, and no per-call identity to spoof.

## Identity

The honest arrangement is **the gateway owns identity, the bridge owns grants**.
A browser cannot present a client certificate, so OIDC or mTLS terminates at
Mycelium's gateway (`fpl-auth`, Pocket ID, Okta — whatever the deployment uses)
and this listener stays on loopback behind it.

Two local modes exist and both say what they prove:

| Mode | Flag | Proves |
|---|---|---|
| Loopback | *(default)* | the OS refused everyone else |
| Bearer token | `--token` | the caller knows one shared string — *a deployment, never a person* |

`--origin` allowlists which pages may script a session. A native client sends no
`Origin` and is unaffected. A configured token is required on both planes; the
session also accepts `?token=` because a browser cannot set an `Authorization`
header on a WebSocket. That puts the token in a URL, which is acceptable for a
loopback dev bridge and not for a public one.

## The generated client

`generated/openapi.json` and `generated/client.ts` are produced from the WIT by
[`soil-openapi`](../soil-openapi), and `cargo test -p soil-openapi` fails if they
are stale:

```sh
cargo run -p soil-openapi -- --package soil:records --out crates/soil-web-bridge/generated
```

A skin uses it directly:

```ts
import { BridgeClient, NewRecordsEntry } from "./generated/client";

const bridge = new BridgeClient();
const entry: NewRecordsEntry = {
  key: { collection: "worklog.event", id: "today" },
  schema: "worklog/event@1",
  body: btoa(JSON.stringify({ message: "hello" })),
};
await bridge.upsert({ entry });

const stop = bridge.session({ onEvent: (event) => {
  if (event.kind === "changed") void bridge.scan({ collection: "worklog.event", limit: 50 });
}});
```

The document describes the HTTP surface (`/v1/call`, `/v1/session`, `/healthz`)
with request bodies generated per WIT function. It cannot correlate a response to
a request, which is exactly why the TypeScript is generated from the WIT rather
than from the document.

## Providers: the other direction

A page is not only a consumer. It can volunteer an interface it implements, and
the node can call it:

```jsonc
// page -> node, once per session
{"v":1,"provide":{"id":"p1","packages":["soil:http"]}}
{"v":1,"id":"p1","result":{"ok":{"packages":["soil:http"]}}}

// node -> page, on the same socket
{"v":1,"call":{"id":"~3","interface":"soil:http","function":"fetch","params":{…}}}
{"v":1,"id":"~3","result":{"ok":{"status":200,"finalUrl":"…","headers":[…],"body":"<base64>"}}}

// when the node gives up
{"v":1,"cancel":{"id":"~3"}}
```

The rules, all of which fall out of the manifest:

- **A session may only provide what the Spore asked for.** `[grants]` must name the
  package; a page offering `soil:http` to a Spore that never declared it is
  `denied`, not quietly accepted.
- **One session answers per package.** A second offer is refused with the reason,
  because silently swapping providers makes a caller's next request land
  somewhere its last one did not.
- **Host call ids are `~n`.** `~` is outside the id alphabet a page may use, so a
  page cannot mint an id that collides with one the host is waiting on.
- **Bounded and cancellable.** `POST /v1/provider/call` takes `timeoutMs` (50 ms…60 s,
  default 10 s). The session thread enforces the same deadline, sends `cancel`, and
  a result that arrives late is dropped by id.
- **One call in flight per session.** The session thread owns the socket, so an
  outbound call blocks further outbound calls until it answers. That is a real
  limit, written here rather than discovered later.

`POST /v1/provider/call` is the node-side door:

```sh
curl -s localhost:8795/v1/provider/call -H 'content-type: application/json' \
  -d '{"package":"soil:http","function":"fetch","params":{"url":"https://example.com"},"timeoutMs":5000}'
```

The demo page's **Offer soil:http** button registers and answers with its own
`fetch`, which means the node gets the *browser's* network — its cookies, its
origin allowlist, its CORS — without the node opening a socket.

### Wiring it into a host

The bridge does not run Spores; it is the session side. A process that does both
composes the two pieces, and `SessionTransport` is the adapter:

```rust
use std::sync::Arc;
use soil_http_host::{DirectTransport, HttpHost, HttpPolicy, PreferProvider};
use soil_web_bridge::{Bridge, SessionTransport};

let bridge = Arc::new(bridge);
let transport = Arc::new(PreferProvider::new(
    Arc::new(SessionTransport::new(bridge.clone())),
    Arc::new(DirectTransport),          // the node's own network, when nobody offers
));
let host = HttpHost::new(policy).with_transport(transport);
```

`PreferProvider` falls back **only** on `TransportError::Unsupported`, which is
the transport's way of saying "not me". A provider that declines a URL returns
`denied` and nobody routes around it: a fallback on failure would quietly
overrule the page's own policy.

That composition is tested here (`a_spore_fetch_goes_through_the_session…`) and
in [`soil-http-host`](../soil-http-host/src/lib.rs); what does not exist yet is a
binary that both serves the bridge and runs a Spore. Today that is two commands
in two processes — which is exactly why the seam is a trait rather than a call.

## What is not here yet

- **Pixels.** `soil:present` and `soil:surface-import` are a separate channel with
  their own congestion story. Nothing here should ever carry a frame.
- **More than one interface per bridge.** The document and the client are
  generated per package; the server already dispatches any number, so widening
  this is a CLI flag and a loop, not a protocol change.
