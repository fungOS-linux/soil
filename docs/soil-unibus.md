# `soil:unibus@0.1.0` — the Spore-facing bus edge

Status: **wired, 2026-11-07.** `soil-wasi-host` implements `bus.publish` /
`bus.poll` over `UnibusPublisher` and `UnibusBus`; `MemoryBus` backs tests.

## What the contract is

```wit
package soil:unibus@0.1.0;

interface bus {
  record frame { kind: string, producer: string, issued-at-ms: u64, body: list<u8> }
  record page { frames: list<frame>, next: option<string> }
  record receipt { request-id: string }
  enum error { denied, invalid, over-budget, stale, unavailable }
  poll: func(kinds: list<string>, cursor: option<string>, limit: u32) -> result<page, error>;
  publish: func(kind: string, body: list<u8>, destination: option<string>, ttl-ms: u64)
    -> result<receipt, error>;
}
```

- `body` is opaque JSON-object bytes. The host rejects non-objects as
  `invalid`, enforces the 64 KiB ceiling as `over-budget`, and never reads
  the fields. The Spore owns the schema.
- `kinds` names what a Spore observes. An empty list is `invalid`, never
  "all".
- `producer` is presentation, not authorization. `fungos:http-context` and
  `soil:creds` carry the authenticated facts.
- `receipt` has no `duplicate` flag. Unlike `fungos:events`, there is no
  idempotency key on the WIT. Callers that need one dedupe on content
  (Confluence does, on `provider` + `delivery_id`).
- `ttl_ms` is bounded by the host (`0` = default, 24h ceiling, else
  `invalid`). The Unibus SDK has no per-call TTL knob; the router enforces
  its freshness window.

`fungos:events` stays: one-way publish with an explicit idempotency key and a
`duplicate` receipt. `soil:unibus` is the poll/publish edge. Both may sit on
the same session.

## Sparse hubs: pris, batty, studio, agora-one, titan

Today `unibus-router` is one hub; `ClientConfig` names one `hub_endpoint`.
A fleet of five machines cannot all depend on one hub losing power, and a
client that fails hard when its hub is down is not sparse-tolerant.

The adapter is **client-side failover**, not hub federation:

```text
--unibus-hub  plugs  ordered failover at connect time
                 first hub to register wins; the order is priority

unibus-router   still the only router. Hub-to-hub gossip/relay is Unibus proper,
                 not this WIT, and is a separate change in the unibus repo.
```

Host shape:

```rust
use soil_wasi_host::UnibusBus;

let bus = UnibusBus::connect_any(&base_config, &endpoints)?; // tries in order
bus.publish("confluence.webhook.received.v1", &body, None, 0)?;
let page = bus.poll(&["confluence.webhook.received.v1".into()], None, 32)?;
```

- `connect_any` gives you ordered try, with the winning address on
  `bus.hub_endpoint()` for logs. `UnibusPublisher::connect_any` is the same
  helper for the native `soil-confluence serve` path.
- When no hub is healthy, both operations fail with `unavailable`. That is
  deliberate: the Spore sees a typed failure and the caller can retry
  (Confluence maps it to 503 so the provider redelivers). A silent local
  queue would hide a dead fleet.
- There is no background reconnect in the adapter. If the session dies,
  the bus fails closed until the host rebuilds it. Reconnect-with-backoff
  belongs to the host lifecycle, not the WIT.

Example fleet order (Tailscale-first, LAN fallback):

```text
pris:    192.168.1.39:18799, 192.168.1.40:18799, <studio-tailscale>:18799
 batty:  192.168.1.40:18799, 192.168.1.39:18799, <agora-tailscale>:18799
 studio: <studio-tailscale>:18799, 192.168.1.39:18799
```

Point `--unibus-hub` after the config's `hub_endpoint` in the same priority
order. The first to register wins.

## Poll semantics (the sparse-hub gotcha)

`UnibusBus::poll` is a live tail: it drains the inbound queue without
waiting. Any non-empty `cursor` is therefore `stale` — the router does not
retain frames, so there is no history to resume from. Spores restart from
`None` (live). `MemoryBus` is the retained-log double: cursors are base-10
indices, truncation past 1,000 frames makes head cursors `stale`, and `None`
means "retained head", not index 0.

## Confluence policy (`ConfluenceBus`)

`soil-confluence` wraps any inner bus with `ConfluenceBus`:

- tenant stamping (guest `tenant` overwritten)
- `provider` + `delivery_id` validation from the body
- delivery dedupe (top-level `duplicate` receipt collapses to one fact;
  the guest sees 202 for both first and redelivery)
- `destination` must be empty (fan-out fact, not addressed)
- `poll` delegates so an observer Spore can share the host

`soil-confluence serve-spore --transport bus` wires it; `--transport events`
keeps the interim `fungos:events` Spore running.

## Run

```sh
cargo test -p soil-wasi-host -p soil-confluence -p soil-abi
```
