# `soil:appdata`

Private app storage, distinct from `soil:vfs`, `soil:storage`, and `soil:records`.

| Capability | Scope | Sync | Indexed | Shareable |
|---|---|---|---|---|
| `soil:appdata.local` | one app installation, one device | no | no | no |
| `soil:appdata.synced` | one app, one user, their devices | yes | no | no |
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

## Rules

- AppData is per app, per user.
- Synced AppData is encrypted and replicated through records-core.
- It is not indexed, not cross-app referenceable, and not team-shareable.
- Large assets belong in `soil:storage`; user content belongs in `soil:vfs`;
  durable/searchable data belongs in `soil:records`.
- Install grants the declared local/synced quota once. Quota growth is an
  expansion grant.
