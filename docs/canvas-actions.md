# Canvas action catalog

`fungos:actions/catalog@1.0.0` exposes the host action catalog to a Spore without
granting another execution path.

The host binding lives in
[`crates/soil-canvas-actions-host`](../crates/soil-canvas-actions-host). It links
`list-actions` to `ActionHost::canvas_actions()`. The included guest fixture calls
the import through a real `wasm32-wasip2` component.

```wit
list-actions: func() -> result<list<action>, error>;
```

Each action carries enough for a custom top bar, command palette, or connector
script:

- `id`, `label`, `description`, `shortcut`
- `input-schema` — JSON Schema 2020-12 for arguments and ranges
- `requires-input` — host-computed fast path for click vs form
- `effect`, `idempotency`, `approval`
- `available`, `reason`
- DCP `phases`, `provider`, `catalog-revision`, `state-revision`

## DCP mapping

A DCP action maps directly:

| DCP | Canvas action |
| --- | --- |
| `id` | `id` |
| `title` | `label` |
| `description` | `description` |
| `input_schema` | `input-schema` |
| `phases` | `phases` |
| `safety.idempotent` | `idempotency` projection |
| `safety.requires_final` / `confirmation_required` | `approval` |
| `availability.available` / `reason` | `available` / `reason` |
| `provider.id` | `provider` |
| `catalog_revision` | `catalog-revision` |
| `state_revision` | `state-revision` |

Parameterless DCP actions have an empty object schema and become click buttons.
Parameterized actions keep their schema so a host or script can render bounded
forms (`minimum`, `maximum`, `enum`, `pattern`, etc.).

## Invocation

`list-actions` is a catalog projection. It does not grant execution. Invocation
still goes through `fungos:actions/actions.invoke`, which rechecks the grant,
approval, idempotency and DCP revision binding.

The SDK's `fungos.ui.action_bar_from_catalog` consumes the same shape.
