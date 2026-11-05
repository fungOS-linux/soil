# `soil:http`

Status: **host proof, 2026-11-05.** The direct transport and a real Wasm guest
work against a loopback HTTP server. Iroh/Mycelium route selection is an
explicit transport seam, not yet wired.

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
  method: method,          // get, head
  headers: list<header>,   // host-allowlisted names only
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

`crates/soil-http-host` currently provides:

- origin allowlist (`scheme://host:port`)
- GET/HEAD only
- request headers limited to `accept`, `accept-language`, `user-agent`,
  `if-none-match`, `if-modified-since`, `range`
- no redirects
- per-call body and deadline caps, plus host maximums
- credential handles resolved by `CredentialProvider`; no secret bytes enter
  wasm
- `DirectTransport` using `ureq` with TLS verification

The `CredentialProvider` trait is where **Geode** belongs: the Spore sends a
handle, Geode resolves it inside the host, and the host injects the resulting
header. The direct transport is the first `HttpTransport`; a Geode adapter and
an Iroh transport do not change the WIT or the Spore.

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
cd crates/soil-http-host/fixtures/get
cargo build --target wasm32-wasip2 --release
cd ../../..
target/debug/soil-http --allow http://127.0.0.1:8891 \
  crates/soil-http-host/fixtures/get/target/wasm32-wasip2/release/soil_http_get.wasm \
  http://127.0.0.1:8891/feed.txt
```

The host prints the status and bounded body. The guest sees one
`soil:http.fetch` call and no socket, DNS, TLS, proxy, or credential material.

## Next

- Geode `CredentialProvider`
- Mycelium directory / Iroh `HttpTransport`
- shared router with best-route selection and explicit fallback policy
- conditional GET / ETag at the host layer
- feeds Spore using the same capability with a dotfile-projected AppData config
