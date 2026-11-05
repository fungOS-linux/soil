# Notify UI API

## Entrypoint

`ui/tray.lua` exports `M.render(ctx) -> UI tree`.

The view is a pure projection. It has no WIT imports, no network, no focus
authority and no answer authority.

## Snapshot

```lua
{
  focus: { mode: string?, until_ms: integer?, held_count: integer? }?,
  notifications: { Item }?,
  held: { Item }?,
  history: { Item }?,
  call: Ring?,
}

Item = {
  id: string,
  app_id: string?,
  source: string?,
  title: string?,
  body: string?,
  priority: string?,
  posted_at_ms: integer?,
  read: boolean?,
  held: boolean?,
  sensitive: boolean?,
}

Ring = {
  id: string,
  kind: string?,
  source: string?,
  title: string?,
  phase: string,
}
```

## Actions

| Action | Payload | Emitted by |
| --- | --- | --- |
| `notify.focus` | `{}` | top bar |
| `notify.cleanup` | `{}` | top bar |
| `notify.history` | `{}` | top bar |
| `notify.dismiss` | `{ id: string }` | notification item |
| `notify.act` | `{ id: string, action_id: string }` | notification action |
| `ringer.accept` | `{ id: string }` | ringer |
| `ringer.decline` | `{ id: string }` | ringer |
| `ringer.mute` | `{ id: string }` | ringer |

Focus changes, cleanup scopes, history retention and ring acceptance are
host-side actions. The UI emits intents only.

## Slots

None in this slice.

## Components

None. This is an app Spore.

## Theme/config

Uses the global `fungos.ui` profile and `apps["dev.fungos.notify"]`.

## Stable ids

`notify-root`, `notify-actions`, `notify-title`, `notify-focus`,
`notify-now`, `notify-held`, `notify-history`, `ringer-root`,
`ringer-actions`, `ringer-title`.

## Authority

None in Lua. Notifications come from `soil:notify/inbox`; focus from
`soil:notify/focus`; history from `soil:notify/history`; cleanup from
`soil:notify/cleanup`; active rings from `soil:notify/ringer`. History and focus
settings are differently granted from the live inbox.
