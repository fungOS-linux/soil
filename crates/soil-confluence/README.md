# soil-confluence

Minimal webhook ingress for Unibus. It accepts an external HTTP webhook,
verifies the provider signature, deduplicates the delivery, and publishes one
Unibus fact:

```text
confluence.webhook.received.v1
```

There is no separate Confluence Spore in this slice. Unibus is the pub/sub
contract; Elmers and other consumers subscribe to the kind.

## Run against a local Unibus router

```sh
cargo run -p soil-confluence -- serve \
  --listen 127.0.0.1:8899 \
  --unibus-config /tmp/soil-confluence-e2e/confluence.json \
  --webhook-secret-env CONFLUENCE_WEBHOOK_SECRET
```

The Unibus config is the normal v2 client config:

```json
{
  "installation_id": "confluence-test",
  "producer_id": "confluence",
  "hub_endpoint": "127.0.0.1:18890",
  "token_file": "/tmp/soil-confluence-e2e/confluence.token",
  "zones": ["default"],
  "capabilities": [],
  "schema_version": 2
}
```

Route `confluence.webhook.received.v1` to any holder of a consumer capability,
for example:

```json
{
  "kind": "confluence.webhook.received.v1",
  "target": { "mode": "any_holder", "name": "confluence.consume" }
}
```

Then subscribe with `unibus-watch`:

```sh
unibus-watch /tmp/soil-confluence-e2e/elmers.json confluence.webhook.received.v1
```

## HTTP

```text
GET  /health
POST /webhooks/:provider
```

The request body must be a JSON object and at most 64 KiB. The published body
is:

```json
{
  "tenant": "default",
  "provider": "github",
  "delivery_id": "delivery-1",
  "event_type": "push",
  "received_at_ms": 1790000000000,
  "headers": { "content-type": "application/json" },
  "body": { "action": "opened" }
}
```

A `webhook-id`/`x-github-delivery`/`x-request-id` header is the delivery id.
If none is present, the body hash is used. Duplicate `(provider, delivery_id)`
requests return the original request id and do not publish twice.

The in-memory dedupe table is intentionally small in this slice. A durable
outbox is the next step before production.

## Iroh

A process that cannot speak Iroh can use the relay:

```sh
cargo run -p soil-confluence -- relay \
  --listen 127.0.0.1:8898 \
  --endpoint-id <receiver-endpoint-id> \
  --addr 127.0.0.1:<receiver-udp-port>
```

```text
Worker / local caller -> HTTP relay -> Iroh -> Confluence -> Unibus
```

Start the receiver with `--iroh`; it prints its endpoint id and bound UDP
addresses. `--iroh-peer` pins allowed endpoint ids. `--iroh-allow-any` is for
local tests only.

## Tests

```sh
cargo test -p soil-confluence
```
