# Anycast for Spores

A Spore can opt into a name. Every node running it publishes a short-lived lease
saying "I am here for this kind"; the HTTP gateway resolves a `.myc` host to a
**candidate list** and uses whichever candidate answers. That is what anycast
means here — not a DNS trick, a health-filtered list of replicas.

Running N replicas is the point: a webhook that arrives while one node is
degraded lands on another, and a stopped replica leaves the pool within one
lease lifetime instead of one outage.

## The claim

Opting in is a manifest table, so it travels and is reviewed with the Spore:

```toml
[anycast]
name = "confluence"                  # service name
kind = "confluence"                 # the kind the router joins on
alpns = ["soil/http/1"]             # what this Spore answers on
ttl_secs = 240                      # default 240, hard ceiling 300
```

Absent `[anycast]` means local-only, which is the default. A claim is a
*declaration*, not a permission: `Manifest::validate` rejects the malformed,
and the publisher grant decides what is allowed to route.

## Authorization, not just opt-in

Two different keys, on purpose:

| Layer | Says | Enforced by |
|---|---|---|
| `[anycast]` | this Spore *wants* a name | manifest validation |
| publisher envelope | this package *may* have one | `soil-spore`'s `publisher::verify` |

The envelope is an ed25519 signature over a statement naming `spore_id`,
`version`, `package_digest`, `principal`, `grant_id`, and `not_after`. Soil does
not own the authorization table: the host supplies a `PublisherTrust`, and a
Mycelium-backed implementation answers "is this grant still signed, delegated
and unrevoked". A manifest can be edited by anyone; a signature cannot.

## The lease

`lease::document` renders a verified claim as one observation in the shape
`myceliumd` reads from `~/.mycelium/application-services.json`:

```json
{"schema_version":1,"observations":[{
  "service_id":"service/<node_id>/application/confluence",
  "observed_at":1794200000, "expires_at":1794200240,
  "provenance":{"provider":"soil-spore","observer":"<node_id>"},
  "confidence":"strong",
  "value":{"kind":"confluence","endpoints":["http://10.253.0.6:8788"],
           "attributes":{"spore_id":"dev.fungos.confluence","package_digest":"...",
                         "principal":"...","grant_id":"...","publisher_key_id":"...",
                         "alpns":"[\"soil/http/1\"]"}}}]}
```

Why these attributes: a consumer can ask *which signed package* is answering,
not just which address. An unsigned replica can post the same kind; only a
verified one carries a grant id that resolves.

### Bounds, mirrored from the daemon

`myceliumd` drops a lease it does not like, silently, at the Spore's own edge.
So the bounds are checked twice, and `Manifest::validate` rejects them up front
rather than publishing something that will never route:

| Bound | Value | Consequence if broken |
|---|---|---|
| `ttl_secs` | 1..=300 | the daemon expires it; a longer claim is a renewer that never runs |
| `service_id`, `kind` | lowercase `[a-z0-9-_./:]`, ≤256 B | rejected as an invalid lease identity |
| endpoints | 1..=32, `http https tcp quic iroh` | rejected as an out-of-bounds observation |
| endpoint form | no userinfo, path, query, fragment | rejected: an endpoint is a hint, not a capability |

Expiry is enforced and never renewed by the daemon: an announcer that stops
renewing disappears from the pool within one TTL. Fail-closed is the intended
behaviour, but it means the renewer is load-bearing — it must run on every node.

## Routing

One entry in the gateway's `routes.json`, with no static upstream:

```json
{ "name": "confluence", "hosts": ["confluence.myc"], "service_kind": "confluence" }
```

`service_kind` is the join key between a local name and discovered endpoints.
The gateway probes candidates and drops the ones that do not answer — the
`webhook candidate ... connect failed` line in its log is that probe. A route
with a static `upstream` (as `ai.myc` has) never consults the index; a route
with only a kind always does.

## Transport reality

The gateway dials `http(s)://` endpoints. An `iroh://` endpoint is a *valid*
lease record (`soil:creds`' neighbour crate `soil-iroh-http` speaks it) but the
*mycelium* gateway has no iroh dialer today — `strings mycelium-gateway` knows
`unsupported scheme` and nothing more. So:

- **today:** publish mesh-reachable `http(s)://<mesh-ip>:<port>` and let the
gateway do the anycast. The Spore must bind the mesh address, not only loopback.
- **with the dialer:** `iroh://<endpoint_id>` records work cross-WAN without mesh
  reachability, which is the same transport `soil-iroh-http proxy` already
  implements in its own process.

## Status

| Piece | State |
|---|---|
| `[anycast]` manifest table + validation | written |
| `lease::document` + bounds tests | written |
| renewal CLI (`announce`) and Geode/publisher trust wiring | not built |
| `routes.json` anycast entry for `confluence.myc` | not configured |

Until the renewal CLI exists, nothing publishes a `confluence` lease, and
`confluence.myc` must keep a static upstream.
