# Dependencies

Every build dependency is one of:

- crates.io
- a public git repository pinned by full revision
- another package inside this checkout

There are no build-time dependencies on sibling worktrees. The legacy S22
Canvas owner adapter is vendored as `crates/s22-canvas-surface`; its own
dependencies are public git pins, not paths into another checkout.

Check the invariant with:

```sh
./scripts/check-path-deps.sh
```
