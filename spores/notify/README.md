# Notify Spore

Status: **tray and ringer contracts, 2026-11-05.**

One Spore, one WIT package:

```text
soil:notify
  publisher, inbox, focus, history, cleanup
  ringer (calls, doorbell, intercom, alarm, timer, pager)
```

The tray is durable/deferrable. The ringer is active and answerable. They share
host attention policy, not lifecycle ownership.

## Module actions

```text
inbox
history
focus
clear [scope] [value] [older-than-ms]
```

## UI

- notification tray: Now / Held by Focus / Earlier
- focus indicator and top action bar
- cleanup and history intents
- ringer view with Mute / Decline / Accept

History is opt-in through the separate `soil:notify/history` grant.
