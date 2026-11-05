# soil-sync-host

The shared `unibus/peer/1` host for fungOS records and future application
kinds. A Spore imports `soil:records`; it never imports this crate, Unibus, Iroh,
or a peer transport.

## Authorization

There is no separate Unibus session issuer. The retained trust document is a
Mycelium `AccessRecord` grant signed by an existing Mycelium access key. Its
single `unibus-session:SHA256` scope commits to a permit containing:

- both Iroh endpoint keys
- both producer ids
- audience and installation
- exact message kinds
- maximum payload size

`GrantVerifier` verifies that signed grant and synthesizes a short-lived Unibus
session locally. A copied session JSON cannot widen the grant. The permit is
issued with the same Mycelium access authority the fleet already uses for other
signed grants; the host does not mint a new root.

## Commands

```sh
# Persistent endpoint key. One per host; do not reuse the Mycelium daemon key.
soil-sync keygen ~/.config/fungos/soil-sync/endpoint.key --write
soil-sync endpoint-id ~/.config/fungos/soil-sync/endpoint.key

# Retained permit signed by the Mycelium access key.
soil-sync permit \
  --key ~/.mycelium/access.key \
  --out ~/.config/fungos/soil-sync/records-trust.json \
  --audience unibus:soil-records \
  --installation home \
  --initiator-producer records.macbook \
  --initiator-endpoint <macbook-iroh-endpoint-id> \
  --responder-producer records.studio \
  --responder-endpoint <studio-iroh-endpoint-id> \
  --not-after <unix-seconds>

soil-sync run --config ~/.config/fungos/soil-sync/records-studio.json
# Debug/manual only:
soil-sync sync --config ~/.config/fungos/soil-sync/records-macbook.json \
  --peer records.studio --book personal/avery/contacts
```

`issuers` in the config are the explicit public keys of the Mycelium access
issuer set. They are never accepted from a connecting peer.

`run` is the normal mode. It configures the host-owned book list, serves
`unibus/peer/1`, watches a durable per-peer outbox, and runs anti-entropy.

Local records writes commit locally first. The same SQLite transaction appends
the op, projects it, and inserts outbox rows for configured peers. A background
dispatcher pushes pending ops over the shared peer session. Failed pushes stay
in the outbox with a retry time; periodic vector exchange repairs lost acks,
partitions, and concurrent writes. Remote-applied ops are never echoed.

```json
{
  "books": ["personal/avery/contacts"],
  "sync_interval_secs": 300,
  "push_poll_ms": 500
}
```

## Live deployment

The current qualification uses a direct Iroh address over the tailnet:

- studio binds `100.108.9.93:18793`
- macbook binds `100.120.101.5:18794`
- macbook reads studio's endpoint id and direct address from its config

Studio runs `soil-sync serve` under `dev.fungos.records-iroh`. The old
`dev.fungos.records` TCP service and `dev.fungos.records-tunnel` SSH forward are
disabled but retained as `.plist.disabled` rollback files.

Relays are disabled until a relay origin is explicitly allowed in both
configs. Discovery supplies reachability only; the signed grant supplies
authorization.

## Tests

```sh
cargo test -p soil-sync-host
```

The tests run real encrypted Iroh loopback sessions. One uses a Mycelium-signed
access grant and local permit, then syncs a contact record through the shared
host.
