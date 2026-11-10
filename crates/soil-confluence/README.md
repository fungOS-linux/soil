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
provider semantics: it verifies the provider's signature through `soil:creds`
(the secret never enters wasm), checks clock skew against the host-attested
receive time, and emits one `soil:unibus` publication.

## Verifiers are a table

Every provider signs with HMAC-SHA256; what differs is where the signature is,
how it is encoded, what it covers, and where the delivery id comes from. That is
a row, not a branch:

| Provider | Header | Value | Covers | Delivery id |
|---|---|---|---|---|
| `github` | `x-hub-signature-256` | `sha256=<hex>` | body | `x-github-delivery` |
| `gitea` | `x-gitea-signature` | `<hex>` | body | `x-gitea-delivery` |
| `standard-webhooks` | `webhook-signature` | `v1,<base64>` | `id.timestamp.body` | `webhook-id` |

The Spore picks the row by which signature header is present. A missing
delivery id is `401`: without one there is nothing to dedupe a redelivery on,
and the event would be accepted twice.

Adding a provider means adding a row. The table is compiled into the guest
rather than configured because Lua is not available inside a WASI guest; when it
must become configuration, the host passes the row through a capability.

## Webhook registry

```sh
soil-confluence webhook create --name "github avery/soil" --provider github
soil-confluence webhook list
soil-confluence webhook revoke <slug>
```

One JSON document per hook, named by its slug, with its secret beside it. A
created hook is addressable at `/webhooks/<provider>/<slug>` and is loaded by
`--webhook-registry <dir>`: every slug's secret becomes a `soil:creds` name, so
adding a hook needs no restart to rotate a secret.

The directory is a **stand-in for `soil:creds` backed by Geode**. Secrets
belong in Geode; the registry should keep names, not material. See
[docs/soil-creds.md](../../docs/soil-creds.md).

## Origin auth

A webhook can arrive directly, or through a TLS terminator that has already
authenticated the client certificate. The two cases cannot be told apart from
the request, so it is declared:

```sh
--trusted-terminator 10.253.0.6 --trusted-terminator 100.80.85.86
```

With a terminator declared, every other peer is refused — a direct hit on a
terminated listener is exactly the origin bypass the policy exists to stop. The
assertion headers are stripped, and the chosen mode is logged as
`verifier="provider-signature"` or `verifier="trusted-terminator"`. Without
the flag nothing about the transport is asserted and each Spore verifies the
provider's own signature.

## Deployment

Three units, one host (agora-one in the reference setup):

| Unit | Role |
|---|---|
| `confluence-spore` | this process, on `127.0.0.1:8788` |
| `unibus-router` | the hub the Spore registers with; the Spore is down without it |
| `confluence-tunnel` | `cloudflared` for the public name |

The Spore **fails closed** if no hub answers — it exits rather than accepting
events it cannot publish. Order the unit after the hub, or give it
`--unibus-hub` failover endpoints.

`Host` must be granted as it will be seen. The Mycelium HTTP gateway rewrites
`Host` to the upstream address, so a name routed through it is granted as
`127.0.0.1` as well as its public name.

## Console (planned)

A Spore with a Lua UI, following `dev.fungos.notify`: a table of hooks, per-hook
accept/reject counts, last delivery, and the reason for a rejection. Data splits
three ways, one source per fact:

| Store | Holds |
|---|---|
| `soil:records` → `hooks` | slug, provider, tenant, url, created, **credential name** |
| `soil:records` → `deliveries` | slug, delivery id, status, reason, bytes, received at, node |
| `soil:appdata` | the console's own filter, sort, and `changes` cursor |

No secret is in any of them: the credential name is an address. The ingress has
to write the `deliveries` rows first — today nothing counts deliveries, which is
also why the hub logs "matched no live node" for `confluence.consume`.


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

