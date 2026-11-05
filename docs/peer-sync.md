# Shared peer sync and Spore opt-in

Status: **live Iroh cutover, 2026-11-05.** Records sync over
`unibus/peer/1` with a Mycelium-signed access grant. The SSH-forwarded TCP hub
is disabled but retained as an explicit rollback path.

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

`crates/soil-sync-host` is the shared Iroh host. It owns:

- an `unibus_iroh::IrohListener`
- an `IrohTransport` client helper
- a `fungos_records::Store`
- a per-session router that sends `records.sync.*` to the records frame handler

The host can enable sync for one or more approved books while constructing the
`soil:records` adapter. Geode adds its own `geode.*` handler to the same router.
The test suite performs real encrypted Iroh round trips, including one whose
session comes only from a Mycelium-signed access grant and local permit.

There is no separate Unibus session issuer. The retained trust document is a
Mycelium `AccessRecord` grant signed by an existing Mycelium access key. Its
scope commits to both Iroh endpoint keys, producers, audience, installation,
message kinds, and payload bound. `GrantVerifier` synthesizes a short-lived
session from that signed permit; a copied session JSON cannot widen it. The host
deliberately does not accept a permissive JSON session assertion.

`soil-sync` is the runnable host. See
[`crates/soil-sync-host/README.md`](../crates/soil-sync-host/README.md) for key
generation, permit issuance, config, and the live service layout.

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
