# The five marbles (filing blocked)

The tracker could not be written to: `marbles` reports
`OIDC refresh HTTP 400: session expired`. Run `marbles login` and these become
five marbles; the text below is the intended content, kept here so the plan
survives the session. Items 1-3 are landed in this worktree already — the marble
text is written as the record of *what* was built.

## 1. Freeze `soil:surface` (landed)

Rename `soil:canvas` to `soil:surface`; input types come from
`soil:input/types.input` rather than being restated; theme moves behind a
`theme-generation` counter so a host can swap themes without a re-render
contract change. Every host and every spore sits on this interface, so nothing
above it is worth building until it stops moving.

## 2. Keys + virtualization in soil-ui (landed)

`ui.scope` emits `group` markers with stable keys, so a command carries the
identity of the component that produced it. `ui.list` emits only the rows that
fit the granted extent plus overscan; commands per frame stay flat as N grows.
Bench asserts both.

## 3. Spore skeleton (landed)

`crates/spore-guest`: a Rust library a spore links, exporting the surface world
(`observe` / `present` / `input`) and emitting `soil-scene`. The same view that
runs in the host runs in the spore.

## 4. Charts as shape B (to discuss)

`soil:chart` — or scene primitives carrying reduction parameters — so a spore
sends a series plus parameters and the host does GPU decimation. The existing
Grafana/Soil Viz decimation code moves behind that interface without a rewrite.

## 5. `wasi:webgpu` as a spike, not a dependency (to discuss)

Async plumbing first, narrow subset second, and nothing blocks on it. If it
works, chart spores can own their own device; if not, shape B already ships.

---

## Perf notes behind item 2

### Command identity: keys + command-stream diff

Give every command a stable key (reusing `ui.scope`/hit ids where present) so a
frame's commands can be matched against the previous frame's. Diff in Rust, not
Lua: unchanged nodes reuse cached geometry and shaped text, only changed nodes
are rebuilt. The bench must assert cache hit rate, not just correctness.

### Caches behind the command stream (shaping, layout, scene)

Shaping cache keyed by `(text, size, font, wrap)`; layout memo keyed by a
structural hash plus extent; geometry reused for unchanged commands. The Floem
lesson: the linear cost is in the engine, so fix it behind the same view API.
Invariant: shaping calls per frame flatten with row count once warm.

### Virtualized lists as a primitive

Rows emit only what fits the granted extent plus a screen of overscan; the spore
owns scroll offset and extent. Commands per frame stay flat as N grows. Extends
SOIL-2gqd (scroll/clip containers).
