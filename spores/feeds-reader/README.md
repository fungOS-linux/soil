# Feeds reader Spore spike

This is the reader half of the RSS spike.

```text
host feed poller  ->  fungos-records book  ->  soil:records  ->  this Spore
                                          \-> soil:search
settings          ->  soil:appdata (private, synced)
```

The Spore deliberately has **no network capability**. A Spore that scraped URLs
directly would need a new ambient network grant. Fetching belongs to a host-side
poller or an explicit future `soil:http` capability.

Config is a JSON key in the Spore's synced AppData:

```json
{
  "collection": "feed",
  "limit": 50,
  "title": "Feeds"
}
```

The host maps that collection to an approved records book. The Spore never sees
the book id or a host path.

Build:

```sh
rustup target add wasm32-wasip2
cd spores/feeds-reader
cargo build --target wasm32-wasip2 --release
cp ../../target/feeds-reader/wasm32-wasip2/release/feeds_reader_spore.wasm \
  module.wasm
```

The host-side item producer is:

```sh
records import-rss --book personal/avery/feeds --feed https://example.com/feed.xml
records sync-feeds --config ~/.config/fungos/feeds.toml
```

## Why AppData is not a dotfile

`soil:appdata` is intentionally opaque and key-scoped; the Spore cannot name a
host path. If you want human-editable dotfiles, add a **host projection** that
mirrors the AppData key to/from a git-backed file. Do not make the Spore read
the dotfile directly, and do not let the host and AppData both silently win:
choose one authority and use generation checks for writes.
