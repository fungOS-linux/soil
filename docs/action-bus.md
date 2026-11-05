# Host action bus

Spores do not own connector libraries. A Spore names a granted action; the host
routes it to an Unibus adapter.

```text
Spore / Elmers workflow
  -> fungos:actions
  -> soil-actions-host
       grants, approval, idempotency, audit
  -> Unibus action adapter
       unibus-mail, unibus-beeper, ...
```

The WIT contract is [`wit/fungos-actions.wit`](../wit/fungos-actions.wit). The
host implementation is [`crates/soil-actions-host`](../crates/soil-actions-host).

## Descriptor

Every action declares what a durable workflow needs to know:

- exact `id` and `version`
- effect: `read`, `write`, or `external`
- idempotency: `idempotent`, `keyed`, or `unsafe`
- approval: `none`, `operator`, or `destructive`
- input and output schemas

A grant is exact-version. Granting `unibus.mail.send@1` does not grant `@2`.

## Invocation

The caller supplies an action id, version, JSON input and optional idempotency
key. The host checks:

1. the exact-version grant
2. the descriptor and approval class
3. whether an unsafe action may run automatically
4. the durable idempotency receipt
5. the adapter boundary

A keyed receipt is stored in SQLite. Repeating the same action/version/key returns
the original output with `replayed=true`; the adapter is not called twice.

Remote/applied effects are never retried just because a caller lost the reply.
The receipt is the durable answer.

## Boundaries

- A Spore never imports Unibus, opens a socket, or receives a credential.
- The adapter owns the connector protocol and resolves Geode handles locally.
- The host cannot infer credentials from the action name.
- Approval is checked before the adapter is reached.
- Unsafe actions are denied by default; a host may opt in only for an
  operator-driven, non-retrying run.
- The action catalog returned to a Spore is already filtered by local grants.

## Canvas projection

`fungos:actions/catalog@1.0.0` exposes a presentation projection of the
same granted catalog. It preserves DCP `input_schema`, phases and revisions so a
Spore can build its own top bar or parameter form without opening a connector.
See [Canvas action catalog](canvas-actions.md).

## Elmers

The Elmers Spore imports `fungos:actions`. Its UI emits workflow actions as
intents; the host action router maps them to Unibus actions. Connector churn
stays in Unibus adapters, not in every workflow.
