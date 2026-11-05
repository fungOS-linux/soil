# soil-notify-host

Host-owned broker for `soil:notify`, including its `ringer` interface.

This is a host crate, not a Spore.

```text
Spore UI / publisher Spore
  -> soil:notify/* including soil:notify/ringer
  -> soil-notify-host
       app identity, inbox state, focus policy, history, cleanup, ring claim
  -> host Unibus adapter
       notify.publish.v1 / notify.ack.v1 / notify.state.v1
       ringer.incoming.v1 / ringer.accept.v1 / ...
  -> unibus/peer/1
```

A Spore never imports Unibus or Iroh. The host owns the peer session and routes
these message kinds through `PeerRouter` / the durable outbox. The Spore sees
only the capability WIT.

## Why not a Spore broker?

- app identity comes from the signed Spore manifest, so the broker must be host-side
- focus/quiet hours and breakthrough rules are device policy, not app policy
- inbox/history state is per-device and survives the app
- the ringer answer claim is a host lease, not a guest-owned row
- Spores have no Unibus, sockets, filesystem or durable cross-app store by default
- one Spore cannot export a capability that another Spore imports in this runtime

The Spore remains the tray/ringer UI. Protocol meaning can live in the Spore;
transport, identity and policy stay in the host.

## Durable state

SQLite tables:

```text
envelopes        immutable notification envelopes
inbox_state      per-device read/dismiss/snooze/act state
focus            per-device attention profile
rings            active ringer lifecycle + answer claim
outbox           per-peer at-least-once delivery with backoff
cleanup_undo     short undo window for scoped cleanup
```

## Unibus kinds

```text
notify.publish.v1
notify.update.v1
notify.withdraw.v1
notify.ack.v1
notify.state.v1
ringer.incoming.v1
ringer.accept.v1
ringer.decline.v1
ringer.end.v1
```

Envelope `id` is the dedupe key. Delivery is at-least-once. Read/dismiss state
replicates back as `notify.state.v1`. A relay stores envelopes, not per-device
visibility decisions.

## Wasmtime wiring

`NotifyRuntime` links `soil:notify/*` including `soil:notify/ringer` plus WASI. The
real `spores/notify/module.wasm` runs against it in
`component::tests::runs_the_real_notify_spore_against_the_host`.

## Unibus wiring

`NotifyPeerAdapter` accepts an authenticated `unibus::PeerSession`:

- `serve_session` routes inbound `notify.*` / `ringer.*` frames
- `flush_session` sends durable outbox rows
- `soil-sync-host --features notify` mounts the inbound handler on the shared
  peer session
- `notify_db` in the sync config points both processes at the same broker DB

## Test

```sh
cargo test -p soil-notify-host
cargo check -p soil-sync-host --features notify --bins
```
