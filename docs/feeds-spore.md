# Feeds Spore spike

Status: **spike, 2026-11-05.** The Spore builds for `wasm32-wasip2` and passes
`soil-spore check`, but a generic host that links both `soil:appdata` and
`soil:records` is still pending.

## Split

```text
host feed poller  ->  fungos-records book  ->  soil:records  ->  Feeds Spore
                                          \-> soil:search
settings          ->  soil:appdata (private, synced)
```

The first Spore spike reads feed items from `soil:records`. The newer scraper
spike adds `soil:http`: the Spore can request a user-configured feed URL, but
the host still owns the origin allowlist, TLS, budgets, credential handle and
transport route. It never gets a socket, DNS API, or credential bytes.

Spike files:

- [feeds-reader](https://github.com/ajmwagar/spores/tree/main/spores/feeds-reader) — reader Spore, AppData config
- [`records`](https://github.com/fungOS-linux/records) — `import-rss` and
  `sync-feeds` write `fungos.feeds/item@1`

Host-side ingest:

```sh
records import-rss --book personal/avery/feeds --feed https://example.com/feed.xml
records sync-feeds --config ~/.config/fungos/feeds.toml
```

The Spore names a collection (`feed`), not a book. The host maps that collection
to an approved records book, so the Spore still cannot choose a store.

## HTTP and routing

The scraper Spore imports `soil:http` and fetches configured feed URLs through
the host. The host owns the origin allowlist, TLS, redirects, response cap,
deadline, credential handle, and transport route. The first transport is direct
`ureq`; an Iroh/Mycelium transport is a host-side `HttpTransport` implementation
that can prefer a service/peer path and use DNS only as policy-permitted
fallback. The Spore never chooses the route.

Credential handles are opaque strings in the AppData config. A host
`CredentialProvider` resolves them; Geode is the intended store. Tokens never
enter Wasm memory.

## AppData config

```json
{
  "collection": "feed",
  "limit": 50,
  "title": "Feeds"
}
```

The key is `feeds/config.json` in the Spore's synced AppData namespace. The host
derives the app identity from the signed manifest; the Spore never supplies an
app id or a host path.

## Dotfile projection

AppData is not a dotfile and should not become one from the Spore's side. If a
human-editable config is wanted, add a **host projection**:

```text
~/.config/fungos/soil/<spore-id>/config.json  <->  synced AppData key
~/.config/fungos/soil/<spore-id>/ui/*.lua     <->  signed package UI / cache
```

Rules:

- one side is authority; do not let the dotfile and AppData both silently win
- writes use `set-if-generation` so an editor race is visible
- the dotfile directory can be a normal git working tree; committing and pushing
  stay external to Soil
- Lua files are still package assets or an explicitly reviewed override, not an
  ambient filesystem grant
- a Spore never receives the dotfile path; the projection is a host concern

The host-side feed list (`~/.config/fungos/feeds.toml`) is separate from the
Spore's AppData because the host performs the network fetch. That is the correct
place for URLs and credentials-adjacent policy.

## Limits

- no generic AppData + records host yet
- no scheduling, ETag/If-Modified-Since, or conditional fetch
- no feed-specific search ranking yet; items are ordinary records and appear via
  `soil:search`
- no two-way dotfile projection implementation; the boundary is designed above
