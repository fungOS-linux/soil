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
```

The built `fixtures/echo.wasm` is used by `tests/http_service.rs` to verify the
host listener, request construction, guest call, response outparam, and body
streaming end to end.

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

Still host-owned:

```text
appservice token verification
route allowlist
request size limits
media budget
TLS termination
```

Those belong in a bridge-specific host policy layered on top of this generic
WASI profile, not in the Spore.

## Limits

```text
HTTP/1.1 only       no HTTP/2 server yet
no TLS listener     terminate TLS in the host/proxy
no per-route auth   add a bridge policy hook
no body cap yet     add before exposing to untrusted bridges
no graceful stop    host process restart is the current lifecycle
```

These are host features, not reasons for Spores to bypass WASI.
