# `soil:http`

Status: **host + Geode path credentials + desktop presenter, 2026-11-05.** The
direct transport and a real Wasm guest work against a loopback HTTP server,
Geode leases path credentials into the v1 URL slot, and `canvas-winit` grants
the same policy to a GPUI Spore. Iroh/Mycelium route selection is an explicit
transport seam, not yet wired.

## Why not ureq/hyper/reqwest inside the Spore

`ureq`, `hyper`, and `reqwest` assume a native socket stack (or Tokio's I/O
driver). `wasm32-wasip2` has no ambient `std::net`; `reqwest`'s wasm path is for
`wasm32-unknown-unknown` and delegates to browser `fetch`. Dropping one of them
into a Spore would either fail to build for this target or require the host to
provide a network backend anyway.

The WebGPU/cpal shape is the correct one:

```text
Spore          soil:http WIT (URL, allowed headers, credential handle, budgets)
   │
Host           allowlist + credential resolver + transport router
   ├─ direct   ureq / TLS / DNS
   └─ iroh     Mycelium peer or service endpoint, no DNS dependency
```

The guest uses `soil-sdk-http` or the generated WIT bindings. The host owns the
socket, DNS, TLS, redirect policy, proxy policy, credentials and response cap.

## Contract

```wit
record request {
  url: string,
  method: method,          // get, head, post, put, patch, delete
  headers: list<header>,   // host-allowlisted names only
  body: option<list<u8>>,  // writes only; a body on get/head is rejected
  credential: option<string>, // opaque Geode handle, never a token
  max-bytes: u32,
  timeout-ms: u32,
}

record response {
  status: u16,
  final-url: string,
  headers: list<header>,
  body: list<u8>,
}
```

Errors are typed: `denied`, `invalid`, `over-budget`, `timeout`, `unavailable`,
`tls`, `http`, `io`, `unsupported`.

## Host policy

`crates/soil-host` currently provides:

- origin allowlist (`scheme://host:port`)
- `get`, `head`, `post`, `put`, `patch` and `delete`; a body on a read is
  rejected, and a request body shares the same byte budget as a response
- request headers limited to `accept`, `accept-language`, `content-type`,
  `user-agent`, `if-none-match`, `if-modified-since`, `range`
- no redirects
- per-call body and deadline caps, plus host maximums
- credential handles resolved by `CredentialProvider`; no secret bytes enter
  wasm. A provider may resolve a header or a URL path segment. The guest uses
  the literal `__CREDENTIAL__` marker in a path when the host should substitute
  the value; the response's `final-url` is redacted back to the marker
- `DirectTransport` using `ureq` with TLS verification

The `CredentialProvider` trait is where **Geode** belongs: the Spore sends a
handle, Geode resolves it inside the host, and the host injects the resulting
header or path segment. `GeodeCredentials` is the shipped path adapter; the
direct transport is the first `HttpTransport`, and an Iroh transport does not
change the WIT or the Spore.

## Capability origins

`canvas-winit` can bind a stable capability hostname to a configured origin:

```sh
canvas-winit lights.wasm \
  --hue-bridge http://192.168.10.15 \
  --geode-socket ~/.geode/geode.sock --geode-purpose hue.control
```

The guest requests `http://hue.local/api/__CREDENTIAL__/...`. The host
allowlists `http://hue.local`, rewrites it to the configured bridge in its
`HttpTransport`, then leases and substitutes the Geode path segment before the
request leaves the process. The Spore never learns the bridge IP or the key.

## Iroh / Mycelium routing

The host-side seam is:

```rust
pub trait HttpTransport: Send + Sync {
    fn fetch(&self, request: &TransportRequest)
        -> Result<TransportResponse, TransportError>;
}
```

A routed transport can try, in host policy order:

1. an explicit Mycelium/Iroh service endpoint for the target
2. a Mycelium peer with a healthy Iroh path
3. direct DNS/TLS as fallback

The Spore never chooses a route, sees an endpoint, or learns whether DNS or Iroh
was used. Mycelium remains the authority for peer identity and health; Iroh is a
path. An unavailable route is a typed failure, not a silent downgrade to an
untrusted address. For a feed URL whose host is not a known Mycelium target, the
host can either allow direct fetch or deny the origin; that is host policy.

## Smoke proof

```sh
# direct transport + a real guest component
cd crates/soil-host/fixtures/get
cargo build --target wasm32-wasip2 --release
cd ../../..
target/debug/soil-http --allow http://127.0.0.1:8891 \
  crates/soil-host/fixtures/get/target/wasm32-wasip2/release/soil_http_get.wasm \
  http://127.0.0.1:8891/feed.txt
```

The host prints the status and bounded body. The guest sees one
`soil:http.fetch` call and no socket, DNS, TLS, proxy, or credential material.

## CLI

`soil-http` runs one component and prints what it returns:

```sh
soil-http --allow http://192.168.1.10:80 \
  spores/hue/module.wasm --bridge http://192.168.1.10 --key "$HUE_KEY" on
```

For an origin that needs a bearer token, `--bearer-env` reads it from the host
environment and answers the guest's `default` credential handle. The token never
enters argv or the guest:

```sh
soil-http --allow https://api.spotify.com:443 \
  --bearer-env SPOTIFY_TOKEN \
  spores/spotify/module.wasm --device raspotify play
```

For a path credential (Hue's v1 bridge key is the motivating case), point the
host at the owner-only Geode broker and name a purpose:

```sh
soil-http --allow http://192.168.1.10:80 \
  --geode-socket ~/.geode/geode.sock --geode-purpose hue.control \
  spores/hue/module.wasm \
  --bridge http://192.168.1.10 --credential geode://home/hue/bridge status
```

The guest URL contains `/api/__CREDENTIAL__/...`; the host leases the secret for
one fetch, percent-encodes it as one path segment, substitutes it, and redacts
the value from `final-url`. `geoded` is read-only over its socket; storing the
key is the operator `--put` step, followed by a broker restart so it reloads the
store.

## Next

- Mycelium directory / Iroh `HttpTransport`
- shared router with best-route selection and explicit fallback policy
- conditional GET / ETag at the host layer
- feeds Spore using the same capability with a dotfile-projected AppData config
