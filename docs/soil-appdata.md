# `soil:appdata`

Private app storage, distinct from `soil:vfs`, `soil:storage`, and `soil:records`.

| Capability | Scope | Sync | Indexed | Shareable |
|---|---|---|---|---|
| `soil:appdata.local` | one app installation, one device | no | no | no |
| `soil:appdata.synced` | one app, one user, their devices | records sync | no | no |
| `soil:vfs` | selected user/workspace roots | no | host/derived | book policy |
| `soil:storage` | content-addressed cache | no | no | no |
| `soil:records` | structured books | yes | yes | yes |

## Guest SDK

```rust
use soil_sdk::appdata::synced::Json;

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Settings {
    theme: String,
    rows: u32,
}

let settings = Json::<Settings>::open("settings")?;
let mut value = settings.load_or_default()?;
value.theme = "dark".into();
settings.save(&value)?;
```

Local-only value:

```rust
use soil_sdk::appdata::local::Json;
let cache = Json::<SearchCache>::open("recent")?;
cache.save(&cache_value)?;
```

Generation-aware update:

```rust
let Some((mut value, meta)) = settings.load_with_meta()? else {
    return Ok(());
};
value.rows = 80;
match settings.save_if_generation(&value, meta.generation) {
    Ok(_) => {}
    Err(soil_sdk::appdata::Error::Conflict) => {
        // Reload and retry, or present the conflict.
    }
    Err(error) => return Err(error.into()),
}
```

## What "synced" means

`soil:appdata.synced` is replicated as a private records book:

```text
per user + per app
  -> fungos-records op log
  -> records.sync.* transport (Unibus peer session or SSH bootstrap)
  -> another device for the same user
```

It is **not** Git. It is **not** content-addressed. A key is not a CID, and no
repository, URL, or object-store path is exposed to the Spore.

It is also not encrypted end to end yet. `fungos-records` reserves the book-key
wrapping boundary, but its current slice stores JSON in SQLite and relies on the
transport for confidentiality. Do not describe synced AppData as E2EE until
records-core lands book-key wrapping and the host passes an unlocked book key.

The local scope is device-only. `sync-state` for a synced scope is
`not-ready` when the host has no user identity, `offline` before the first
successful peer exchange, `syncing` while the outbox has pending operations,
and `synced` only after the host records a completed exchange.

## Host contract

```wit
enum scope { local, synced }
enum sync-state { local, syncing, synced, offline, not-ready }

open(scope) -> result<store, error>
store.get / get-meta / set / set-if-generation / delete / list-keys
store.generation / sync-state
```

The host derives app identity from the signed Spore manifest. There is no app
id, host path, table name, or SQL in this boundary.

The host adapter is [`crates/soil-appdata-host`](../crates/soil-appdata-host).
It opens one device-only book and, when a user identity exists, one synced book
under `<root>/apps/<app>/users/<user>/synced`. The native records runner calls
`mark_sync_started` / `mark_sync_complete` / `mark_offline`; the Spore cannot
advance that state.

## Rules

- AppData is per app, per user.
- Synced AppData is replicated through records-core. Encryption is deferred
  until records-core lands book-key wrapping; do not claim E2EE today.
- It is not indexed, not cross-app referenceable, and not team-shareable.
- Large assets belong in `soil:storage`; user content belongs in `soil:vfs`;
  durable/searchable data belongs in `soil:records`.
- Install grants the declared local/synced quota once. Quota growth is an
  expansion grant.
