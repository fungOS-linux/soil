# viewer-spore

Core state machine for the Soil Viewer Spore. It contains no browser, network,
display or capture authority; the host owns those objects behind `soil:view`.

The public contract is `wit/soil-view.wit`. The design and acceptance criteria
are in [`docs/viewer-spore.md`](../../docs/viewer-spore.md).

```sh
cargo test -p viewer-spore
```
