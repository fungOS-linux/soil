# Consent broker

`soil-consent` is the host-owned, blocking consent boundary. It is not a second
ACL and it is not Unibus-only.

```text
Unibus grant request ─┐
Spore Store install  ─┤
outbound mail/chat   ─┼─> ConsentBroker ─> system consent surface
Spore capability ask ─┘         │
                                └─ blocks until allow/deny/timeout
```

The caller checks for an existing valid grant first. The broker is used only
when a grant is missing, stale, escalated, or explicitly per-use.

## Request

A request carries the exact action, host-attested principal, payload digest,
effects, scopes, resources, requested lifetime and an optional preview.

The preview is untrusted presentation data. Approval binds to the payload
digest, never to the rendered preview. The host frames the preview with a
trusted header, canonical facts and the decision controls.

## Decision

The broker blocks until one of:

```text
allow once
allow session
allow persistent
deny
timeout
cancel
```

No UI, no decision, a closed surface or a timeout all deny.

## Boundaries

- The broker does not sign grants.
- The broker does not store the Unibus or Mycelium grant table.
- Unibus still verifies and enforces its own grants.
- Mycelium still owns signed identity/scopes/revocation.
- Spore Store approves installation capability sets; it does not become an ACL.
- The consent surface gets one request and cannot enumerate or approve others.

The first adapter is `unibus-consent`, which lowers an approved request into a
normal `NodeGrant`. A `ScopedGrant`/MCP adapter and Mycelium scope adapter use
the same decision.
