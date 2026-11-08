# soil-confluence

Confluence webhook ingress for Unibus. The ingress is a Spore on the Soil
`wasi:http` surface; this crate is the host policy that runs it and the event
shape consumers depend on:

```text
confluence.webhook.received.v1
```

There is no native HTTP webhook path and no Iroh relay. Both existed while the
ingress was being built, and both are gone: the Spore is the only ingress, so a
signature rule cannot drift between two implementations. Provider ingress at
the network edge belongs to `soil-edge`, which passes ciphertext.

## Shape

```text
provider
  -> soil-edge                L4, SNI, ciphertext only
  -> TLS terminator           Palisade, or the host ingress itself
  -> soil-confluence spore    wasi:http + soil:creds + soil:unibus
  -> Unibus router
  -> any consumer
```

## Run

```sh
cargo run -p soil-confluence -- spore \
  --listen 127.0.0.1:8788 \
  --unibus-config /tmp/soil-confluence-e2e/confluence.json \
  --spore <dir>/confluence.wasm \
  --host confluence.soil \
  --tenant default
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

`--unibus-hub` adds hub endpoints after the config's `hub_endpoint`, tried in
order; the first hub that registers wins. That is the sparse-hub failover.

`--host` is repeatable: one process can answer for several names (an internal
`.soil` name and a public `.myc` name) without a second Unibus session. Note
that the Mycelium HTTP gateway rewrites `Host` to the upstream address, so a
name routed through it must also be granted as `127.0.0.1`; otherwise the Soil
gateway answers `unknown host`.

Route the kind to any holder of a consumer capability:

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

## Published body

The host owns the listener, the route grant (`POST /webhooks/`), the 64 KiB
body ceiling, the Unibus session, and the tenant stamp. The Spore owns the
provider semantics: it verifies the Standard Webhooks signature through
`soil:creds` (the secret never enters wasm), checks clock skew against the
host-attested receive time, and emits one `soil:unibus` publication.

```json
{
  "tenant": "default",
  "provider": "provider-a",
  "delivery_id": "delivery-1",
  "event_type": "page_updated",
  "received_at_ms": 1790000000000,
  "headers": { "content-type": "application/json" },
  "body": { "action": "opened" }
}
```

`tenant` is host policy: a value the guest supplies is overwritten. Redelivery
of the same `provider` + `delivery_id` is collapsed to one fact by
`ConfluenceBus` and the guest sees the original acceptance.

`CONFLUENCE_WEBHOOK_SECRET` is bound to the `confluence.webhook` credential
name by default; `--credential-name` changes it. A missing secret is a startup
error, not an unsigned fallback. The route ceiling is 64 KiB by default
(`--max-body-bytes`), and an over-budget normalized event is `503`.

The in-memory dedupe table is intentionally small. A durable outbox is the
next step before production.

## Tests

```sh
cargo test -p soil-confluence
```

