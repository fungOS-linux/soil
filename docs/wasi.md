# WASI host profile

Soil provides a reusable WASI p2 profile so Spores can use standard interfaces
instead of a custom SDK for every host capability.

```text
crates/soil-wasi-host
```

## Interfaces

```text
wasi:cli
wasi:clocks
wasi:filesystem
wasi:http/incoming-handler
wasi:http/outgoing-handler
wasi:http/types
wasi:io
wasi:random
wasi:sockets
fungos:http-context
soil:creds
soil:unibus
```

WASI describes the shape. Soil supplies the authority. The default policy is
deny:

```text
outbound HTTP    deny all origins
DNS              deny
TCP              deny
UDP              deny
filesystem       no preopens
environment      no inherited variables
stdio            no inheritance
```

## Outbound HTTP

Allowed origins are exact `scheme://host:port` matches:

```rust
let mut http = HttpPolicy::default();
http.allow_origin("https://api.telegram.org")?;

let policy = WasiHostPolicy {
    http,
    ..WasiHostPolicy::default()
};
let builder = WasiHostBuilder::new(policy);
```

A guest may import `wasi:http/outgoing-handler`, but requests outside the
origin set fail with `HTTP-request-denied` before a socket is opened.

## Sockets

Network policy is per use, not a global switch:

```text
tcp-connect
tcp-bind / tcp-listen
tcp-accept
udp-send
udp-receive
udp-bind
dns
```

Each entry is a CIDR:

```rust
let mut network = NetworkPolicy::default();
network.allow_tcp_connect("10.0.0.0/8")?;
network.allow_tcp_connect("127.0.0.1/32")?;
network.allow_dns(true);
```

`wasi:sockets` uses the host address check on every connect, bind, listen,
accept, send, and receive. Raw DNS names are not resolved by the socket layer;
`ip-name-lookup` is a separate grant.

## Filesystem

Files are explicit preopens:

```rust
let builder = WasiHostBuilder::new(policy)
    .preopen_read_only("/srv/config", "/config")
    .preopen_dir("/srv/cache", "/cache", FsPerms::ReadWrite);
```

There is no ambient filesystem. Host paths never appear in the guest.

## Request context

`fungos:http-context` supplies the facts the host owns for one invocation:

```wit
record request-context {
  request-id: string,
  spore: string,
  route: string,
  peer: option<string>,
  tls-peer: option<string>,
  received-at-ms: u64,
}
```

`spore` and `route` come from the host's grant table, not from a request
header. `peer` is the accepted transport address; `tls-peer` is set only after
the listener verifies a client certificate. Calling `current()` outside an
HTTP invocation traps instead of returning a fabricated context.

## Credentials

`soil:creds` keeps secrets in the host:

```wit
resolve: func(name: string) -> result<handle, error>;
sign:    func(handle, algorithm, message) -> result<list<u8>, error>;
verify:  func(handle, algorithm, message, signature) -> result<bool, error>;
```

There is no `read`, `list`, or `export`. `handle.id` is an opaque grant
reference; the provider re-checks the grant and expiry on every operation, so
a guest cannot extend a handle by editing the record it received. HMAC-SHA256
and Ed25519 are implemented by `StaticCredentials`; a Geode adapter implements
the same trait by resolving through Mycelium.

## Events

`soil:unibus` is bounded poll/publish over the Unibus, not a broker client:

```wit
publish: func(kind, body, destination, ttl-ms) -> result<receipt, error>;
poll:    func(kinds, cursor, limit) -> result<page, error>;
```

The guest supplies a kind and a JSON-object body. The host enforces the size
ceiling, deduplicates on content where the policy requires it, and hands the
fact to its `Bus`. Unibus is the production bus; `MemoryBus` is for tests. No
address, session, token, or subscription crosses the boundary.

## HTTP service

`HttpService` runs a component that exports
`wasi:http/incoming-handler`:

```rust
let service = Arc::new(HttpService::from_bytes(&bytes, builder)?);
let listener = TcpListener::bind("127.0.0.1:8080").await?;
service.serve(listener).await?;
```

The host owns:

```text
listener
HTTP/1.1 parsing
request body streams
response outparam
keep-alive
per-request store
policy
```

The guest owns only the handler. A fresh WASI store is created for each request,
so request state cannot leak between requests.

## Vhost gateway

`HttpGateway` puts several Spores behind one listener with explicit grants:

```rust
let gateway = HttpGatewayBuilder::new()
    .max_body_bytes(64 * 1024)
    .vhost(
        "confluence.soil",
        "dev.fungos.confluence",
        service,
        [GatewayRoute::new([Method::POST], "/webhooks/")?],
    )?
    .build()?;
```

Each request is resolved by Host header, then by the longest matching path
grant, then by method. Unknown hosts return `404` without naming the grant
table; a known path with the wrong method returns `405` and `Allow`. A declared
`Content-Length` over the route ceiling is rejected with `413` before the guest
runs. Chunked bodies stop at the ceiling and surface a body-stream error to the
guest, never a truncated body. The gateway sets `HttpContext` for the chosen
vhost and route.

## CLI

```sh
soil-wasi-http \
  --listen 127.0.0.1:8080 \
  --allow-origin https://api.example.com \
  --allow-tcp-connect 10.0.0.0/8 \
  --allow-dns \
  --inherit-stdio \
  component.wasm
```

Useful flags:

```text
--allow-origin ORIGIN
--allow-tcp-connect CIDR
--allow-tcp-bind CIDR
--allow-tcp-accept CIDR
--allow-udp-send CIDR
--allow-udp-receive CIDR
--allow-udp-bind CIDR
--allow-dns
--arg VALUE
--env KEY=VALUE
--inherit-env
--inherit-stdio
--dir HOST:GUEST[:ro|rw]
```

## Fixture

```text
crates/soil-wasi-host/fixtures/echo/
  wasip2 component exporting wasi:http/incoming-handler
crates/soil-wasi-host/fixtures/context/
  reads fungos:http-context over the same surface
crates/soil-wasi-host/fixtures/confluence/
  verifies soil:creds and publishes soil:unibus
```

The built `fixtures/echo.wasm` is used by `tests/http_service.rs` to verify the
host listener, request construction, guest call, response outparam, and body
streaming end to end. `fixtures/context.wasm` proves the context import.
`fixtures/confluence.wasm` is the first full ingress: a Standard Webhooks
signature check through `soil:creds`, a host-attested request id and receive
time, and exactly one `confluence.webhook.received.v1` fact through
`soil:unibus`.

## Confluence ingress

```sh
cargo run -p soil-confluence -- spore \
  --listen 127.0.0.1:8787 \
  --unibus-config /tmp/confluence.json \
  --spore crates/soil-wasi-host/fixtures/confluence.wasm \
  --host confluence.soil \
  --tenant tenant-a
```

The host reads `CONFLUENCE_WEBHOOK_SECRET`, resolves it as the
`confluence.webhook` credential, and binds the vhost to the compiled Spore. The
Spore verifies `webhook-id.webhook-timestamp.body`, rejects more than five
minutes of clock skew, normalizes the delivery, and publishes one event. The
host stamps `tenant`; a value from the guest is overwritten. TLS termination,
SNI routing, Iroh transport, and the Unibus session stay in the host.

## Bridge use

A bridge sink can now be a Spore:

```text
bridge
  -> POST /transactions
  -> soil-wasi-http host listener
  -> wasi:http/incoming-handler
  -> bridge Spore
  -> soil:records
```

The generic profile already supplies vhost, path, method, body-ceiling and
deadline policy. Still host-owned:

```text
appservice token verification
media budget
per-route authentication or mTLS
TLS termination
```

Those belong in a bridge-specific host policy layered on top of this generic
WASI profile, not in the Spore.

## Limits

```text
HTTP/1.1 only       no HTTP/2 server yet
no TLS listener     terminate TLS in the host/proxy
no per-route auth   add an auth policy hook
no response cap     request bodies are capped; responses are not yet
event budget        64 KiB normalized event; CID spill is next
no graceful stop    host process restart is the current lifecycle
```

These are host features, not reasons for Spores to bypass WASI.
