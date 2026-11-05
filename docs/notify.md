# Notifications and ringer

Status: **contracts + Spore + host broker core, 2026-11-05.**

## Split

```text
dev.fungos.notify Spore
  tray, focus controls, history, cleanup, ringer UI

soil:notify + soil:ringer
  capability contracts

soil-notify-host
  host-owned broker: identity, inbox state, focus, history, cleanup, ring claim

Unibus adapter (host)
  notify.* and ringer.* kinds on unibus/peer/1
```

The broker is not a Spore. A Spore cannot import Unibus or Iroh, and a Spore
cannot export `soil:notify` for another Spore to import. App identity, attention
policy and the ring answer claim must stay host-owned.

The notify Spore is one package with two WIT imports. Its grants remain
separate: inbox, focus, history, cleanup and ringer calls.

## Publisher rules

- `app_id` comes from the signed manifest, never the payload.
- publisher priority is advisory; `urgent` is clamped to `active`
- critical interruption requires a separate grant and recipient policy
- actions are registered intents, not URLs or shell commands
- bodies are sensitive by default; no raw body to a cloud LLM

## Focus

Focus is per-device and host-owned. Held notifications stay present in the
Held section; they are not deleted. Breakthrough is an explicit allowlist.

## History

History is opt-in and separately granted. It is not a searchable archive by
default.

## Cleanup

Scoped clears create a short undo receipt. Archive/purge deletion needs an
explicit confirmation and is outside the undo window.

## Cross-device

Envelope `id` is the dedupe key. Source hosts keep durable per-recipient
outboxes until ack. Read/dismiss state replicates as `notify.state.v1`. The
relay stores envelopes, not per-device policy decisions.

## Ringer

A ring is active, not a notification row. First acceptance is a transactional
host claim; only one device may accept. A missed/ignored ring may become a
normal notification only after expiry/decline. Audio/device handles stay
host-owned; the ringer names output intent only.
