# Consent popup

`soil-consent-popup` is the first native consent surface. It is deliberately
replaceable: the broker lives in `soil-consent`, the popup is just presentation
and a Unix-socket client.

```sh
cargo run -p soil-consent-popup
```

The window starts hidden and opens when a pending request arrives. It shows one
request at a time:

- trusted host header
- requester and host-attested principal
- exact action, effects, scopes, resources and requested lifetime
- payload digest
- artifact, Spore-scene or schema preview
- Deny / Allow once / Allow session / Allow persistent

Closing the window, no decision, cancel or timeout denies. The socket is mode
`0600` and per-user by default.

## Wire protocol

Unix-socket, newline-delimited JSON. A client sends one `soil_consent::Request`
and waits for one response:

```json
{"kind":"decision","decision":{"request_id":"...","allowed":true,"scope":"once","reason":null,"decided_by":"native-consent-popup","decided_at_ms":0}}
```

Errors are:

```json
{"kind":"error","error":"..."}
```

The caller still checks for an existing valid Unibus/Mycelium grant first. The
popup is only for a missing, stale or escalated grant.

## Why the broker is a crate

The broker must outlive a UI window, be callable by Unibus/Spore Store before a
Spore is running, and fail closed when no UI exists. The popup is the system
surface; the broker is host policy infrastructure. A future host daemon can own
the broker and the popup can connect as a pure view.
