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
           "attributes":{"node_id":"<node_id>","node_name":"runner-3.somewhere.myc",
                         "site":"somewhere","spore_id":"dev.fungos.confluence",
                         "package_digest":"...","principal":"...","grant_id":"...",
                         "publisher_key_id":"...","alpns":"[\"soil/http/1\"]"}}}]}
```

Why these attributes: a consumer can ask *which signed package* is answering,
not just which address. An unsigned replica can post the same kind; only a
verified one carries a grant id that resolves. `node_name` and `site` are
informational — they are what turn a digest into a host an operator can find —
and are absent when the certificate carried no name.

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

## Unattended enrollment

An ephemeral node is created by `terraform apply` and touched by nobody. So the
three things it needs arrive in a fixed order, and each one is smaller than the
last:

| | **Invite** | **Identity** | **Lease** |
|---|---|---|---|
| Mints | one node | the node's own name and key | one replica's presence |
| Lives | minutes | as long as the node does | seconds |
| Owned by | Mycelium (`invite create`) | Mycelium (`setup --claim`) | soil (`announce`) |

**Soil invents none of the first two.** `mycelium invite create` already stores
only a hash of the claim and honours a TTL and a use count; `mycelium setup
--claim` already writes `pki/node.pem`, `node.env` and the peer service. A
second invitation format would be a second thing to revoke, so there is one.

The node needs a **peer** invite, not an access one: an access invite issues an
SSH certificate, and a node that has no peer certificate cannot publish into
the mesh at all.

```bash
# once, by whoever provisions the fleet
mycelium invite create --kind peer --name runner-3 --site aws-us-east-1 \
    --peer <seed> --ttl 15m --uses 1 --write

# first boot, from cloud-init; the claim is the only secret that is planted
mycelium setup --claim "$(cat /run/mycelium-claim)" --gateway https://… --write

# every boot, under a supervisor. `--unsigned-dev` is the gap: it stands in
# for the publisher grant until Mycelium exposes the trust view (see Status).
soil-spore announce /opt/confluence/spore \
    --endpoint http://10.253.0.6:8788 --unsigned-dev
```

`announce` is never told the node id. It reads the certificate Mycelium already
installed and derives the id from it — a digest of `pki/node.pem` with
whitespace removed. That gives two properties the milestone actually needs:

- **a restarted node reappears as itself**, because the certificate is on disk
  and unchanged, so a spot restart is not mistaken for a flapping replica;
- **a re-enrolled node is a genuinely new node**, which is the intended rule —
  no long-lived state may be bound to a node id, so losing the secret is
  ordinary rather than an incident.

What the lease carries besides the digest: the `.myc` name and site from
`node.env`, when the certificate had a name. A digest alone tells an operator
nothing about which host answered.

### The supervisor is part of the design

A lease expires, so a node that is not renewing is not in the pool. Run
`announce` under systemd or launchd with `Restart=always`; a crash-looping
announcer looks exactly like a node that left, which is the right failure but an
invisible one unless the unit is supervised.

Stopping the unit is the *normal* end of an ephemeral node, so `announce` treats
SIGTERM and SIGINT as a drain: it stops renewing and removes its own observation
at once instead of letting the last lease run its full TTL. Other Spores on the
same node keep their observations — `myceliumd` reads one document, so the
announcer merges rather than clobbers. Give the unit a few seconds:

```ini
[Service]
Restart=always
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec=10
```

A renewal interval at or beyond the lease is refused rather than honoured: such
a node is out of the pool for part of every cycle and looks like a flapping
replica to the router. `--every` defaults to a third of `ttl_secs`.

## Routing

One entry in the gateway's `routes.json`. Without a static upstream there is
nothing to fall back to, so an unpublished name is a 502 rather than a stale
host:

```json
{ "name": "confluence", "hosts": ["confluence.myc"], "service_kind": "confluence" }
```

`service_kind` is the join key between a local name and discovered endpoints.
Candidates are **merged**, not substituted — `candidates()` in
`mycelium-gateway/src/server.rs` orders a route's targets as loopback, then
every discovered service of that kind, then a configured remote upstream, which
stays as the fallback for when no replica is live. So a published lease does
take precedence, and a route may keep its static upstream for the empty case.
The gateway probes candidates and drops the ones that do not answer — the
`webhook candidate ... connect failed` line in its log is that probe.

A discovered peer receives only a credential the operator configured for remote
use (`remote_token_env` / `remote_token_file`), never the caller's; a route with
`service_kind` and no remote credential refuses a discovered target rather than
forwarding the caller's token to somebody else's machine.

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
| `identity`: id from the enrolled certificate | written |
| renewal CLI (`announce`), renewal interval, drain on SIGTERM | written, `--unsigned-dev` only |
| publisher-grant trust (replacing `--unsigned-dev`) | not built; needs the Mycelium trust view |
| `routes.json` anycast join for `confluence.myc` | route present with `service_kind: confluence`; a lease from agora dedupes against the configured upstream of the same URL, so **no lease has yet been seen to change where a request goes** |

Until the grant adapter exists, `announce` refuses unless `--unsigned-dev` is
passed, so a local test cannot be mistaken for a fleet grant. That flag is the
only thing standing between a signed package and a published lease.
