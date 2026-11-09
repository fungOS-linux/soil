# confluence-edge Worker

Cloudflare Worker shim for Confluence. It accepts the provider webhook and
forwards it to a local/public Confluence HTTP-to-Iroh relay.

```text
provider -> Worker -> confluence relay -> Iroh -> soil-confluence -> Unibus
```

A Worker cannot dial Iroh directly: Workers are HTTP/TCP and Iroh is QUIC/UDP.
The relay is the Iroh hop.

## Local

```sh
worker-build --release
wrangler dev --local --port 8792
```

Set `CONFLUENCE_RELAY_URL` in `wrangler.toml` to the relay address.

## Deploy

Set `CONFLUENCE_RELAY_URL` to a public relay URL before deploying. Do not send
provider secrets to the relay; the relay only receives the already-verified
body and forwards it over a pinned Iroh endpoint.
