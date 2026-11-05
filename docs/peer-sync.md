# Shared peer sync and Spore opt-in

Status: **transport-neutral records core, 2026-11-05.** The shared
`unibus/peer/1` Iroh endpoint is the host adapter still being wired.

## One session, many application kinds

```text
Mycelium
  endpoint directory, identity, grants, revocation
  mycelium/sync/1 = gossip/control-plane only

Unibus
  unibus/peer/1 = one authenticated peer session
    records.sync.*
    geode.sync.*
    future application kinds
```

Records and Geode do not get separate Iroh transports. They share the session,
endpoint discovery, identity checks, expiry, grants, and revocation. They differ
only by message kinds.

## Spore opt-in

A Spore does not import `soil:unibus`, `soil:iroh`, or `unibus/peer/1`.

```wit
import soil:records;
import soil:appdata;
```

The host, not the Spore, decides:

- which records book a collection maps to
- whether that book syncs
- which peer/session carries it
- which Mycelium grants and Geode credential handles apply

A Spore names only its collection, schema, and record ids. The host names the
book; the peer session names authenticated producers. There is no Spore-visible
endpoint, DNS name, Iroh key, token, or transport choice.

This is the same rule as `soil:http`: transport in the host, protocol meaning in
the Spore. A Spore cannot silently opt into a new network path.

## Host composition

The host can enable sync for one or more approved books while constructing the
`soil:records` adapter. The background peer runner accepts a `unibus/peer/1`
session and routes `records.sync.*` to the records peer handler. Geode adds its
own `geode.*` handler to the same router.

The host owns:

- endpoint key and relay policy
- Mycelium directory and peer identity verification
- session assertions, audience, installation, and expiry
- grant/kinds allowlist per peer
- Geode credential resolution
- the choice to keep TCP/SSH as an explicit fallback

## Records side

`fungos-records` now exposes transport-neutral:

```rust
serve_peer_session(&mut peer_session, &mut store, &PeerEndpoint { ... }).await
sync_peer_session(&mut peer_session, &mut store, book, &PeerEndpoint { ... }).await
```

The envelope producer/installation ids are supplied by the host. The records
handler never chooses a route or opens a network connection.

## Non-goals

- no Spore-selected peer or endpoint
- no per-app Iroh transport
- no records payload on `mycelium/sync/1`
- no silent downgrade from Iroh to SSH/DNS
- no second identity or grant database
