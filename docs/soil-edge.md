# Soil L4 edge

Status: **first cut, 2026-11-07.** TLS passthrough, exact SNI routes, TCP and
Iroh backends. Signed Mycelium leases, the health side channel and relay-only
Iroh endpoints are next.

## Why this is native, not a Spore

The edge is the bootstrap path to Soil. It must accept a TCP connection, read
one ClientHello, and pipe bytes before any Wasm runtime is reachable. A Spore
cannot own a listener or an ambient socket, and putting the edge inside Soil
would make Soil a prerequisite for reaching Soil.

A Spore could compute the *route* from `(sni, peer)` if that logic ever needs to
be tenant code. The listener, TLS passthrough, Iroh dial and byte pipe stay in
the host. For now the route table is signed data, not code.

## Trust boundary

The edge sees:

```text
TCP connection metadata
TLS ClientHello bytes, including SNI
ciphertext in both directions
```

It does not see HTTP, headers, bodies, provider signatures, or decrypted
traffic. Soil terminates provider TLS and verifies the provider HMAC at the
backend. That is the whole point: a provider webhook reaches Soil without an
intermediate HTTP terminator.

## Cloudflare layer

Use one of:

```text
Spectrum application, tls: off (passthrough)
DNS-only (grey cloud) A/AAAA record to the public edge IP
```

Do **not** use the orange-cloud HTTP proxy. It terminates TLS at Cloudflare and
forwards plaintext HTTP; the edge would see no SNI and Cloudflare would see the
webhook body. Spectrum still terminates the TCP socket at L4, but with
`tls: "off"` the TLS byte stream is untouched. The `tls` field is listed as an
Enterprise setting; confirm the plan.

## Flow

```text
provider :443
  -> Spectrum tls:off or DNS-only
  -> soil-edge :443
       read ClientHello, match SNI
       -> tcp://127.0.0.1:8443          (Soil ingress on this host)
       -> iroh://<id>@<addr>             (Soil ingress elsewhere)
  -> Soil terminates TLS, verifies provider HMAC
  -> Unibus
```

## Run

```sh
soil-edge \
  --listen 0.0.0.0:443 \
  --route confluence.soil=tcp://127.0.0.1:8443 \
  --route crm.soil=iroh://<endpoint-id>@10.0.0.5:4433 \
  --alpn confluence/tls/1
```

Routes are exact hostnames, lowercased with a trailing dot stripped. An SNI
with no route is closed before any backend is contacted. There is no wildcard or
default route.

Backends:

```text
tcp://HOST:PORT              plain TCP, usually a local Soil gateway
 iroh://ENDPOINT_ID@HOST:PORT  direct Iroh address, one bidirectional stream
```

Iroh relay-only endpoints and signed Mycelium endpoint leases are deferred; the
current Iroh route needs a direct address.

The backend **must terminate TLS**. The edge does not, by design. Today
`soil-confluence spore` is still a plaintext HTTP gateway, so pris needs one of:

```text
Caddy/nginx on 127.0.0.1:8443 -> soil-confluence spore on 127.0.0.1:8444
or a rustls listener added to the Soil ingress (next change)
```

Until one of those exists, point the route at whatever TLS terminator is already
on the box.

## Palisade as the TLS terminator

Palisade already has what the backend needs: SNI certificate resolution, a
per-domain route table, optional OIDC/mTLS, and a plaintext upstream mode. The
natural pris chain is:

```text
provider
  -> Spectrum tls:off or DNS-only
  -> soil-edge :443          SNI -> backend, no termination
  -> palisade 127.0.0.1:444   TLS termination + certificate + route
  -> soil-confluence spore 127.0.0.1:8444   plaintext HTTP
```

Palisade config for the hop:

```toml
[[certs]]
domain = "confluence.example.com"
ca_cert = "./certs/ca.crt"
server_cert = "./certs/confluence.example.com.fullchain.crt"
server_key = "./certs/confluence.example.com.key"

[server.confluence]
domain = "confluence.example.com"

[[server.confluence.location]]
path = "/"
addr = "127.0.0.1:8444"
use_tls = false
```

Leave `auth` unset on that location. Provider webhooks authenticate with
their HMAC, not OIDC; adding an `AuthConfig` would reject them before Soil can
verify the signature.

And on the edge:

```text
--route confluence.example.com=tcp://127.0.0.1:444
```

Do not point Palisade **at** `soil-edge` with `use_tls = false`. The edge
accepts TLS bytes, not HTTP. `use_tls = true` makes it work, but that is a
second TLS leg for no benefit when Palisade is already the local terminator.

Palisade does see the webhook body; it is inside the Soil trust boundary on
pris. The provider HMAC is verified again in the Spore, so Palisade cannot
silently alter a delivery into a different valid one. It can drop or replay;
Unibus dedupe and provider retries cover that.

## Limits

```text
client hello    64 KiB default, 10s handshake deadline
backend connect 10s deadline
routes          exact SNI only
Iroh            direct address only; ALPN is one per process
TLS             passthrough only; no termination or certificate handling
HTTP            never parsed
```

## Deploy on pris

The first target is `pris`, a Mycelium-joined lab host behind the commercial
fiber line. Install the unit in [`deploy/soil-edge.service`](../deploy/soil-edge.service),
create the `soil-edge` user, and point Cloudflare at the host (Spectrum
`tls: off`, or a DNS-only record).

A semi-static lab IP is fine for the first cut. Keep DNS TTL low and run a
DynDNS updater (or Spectrum's origin DNS) so an address change is a DNS update,
not a provider reconfiguration. The edge is stateless; it can be replaced or
moved without touching Soil.

## Verify

```sh
cargo test -p soil-edge
```

The end-to-end test proves the backend receives the **exact** ClientHello bytes
and that an unknown SNI never reaches a backend.
