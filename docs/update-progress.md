# Update progress

An update is not one event. Today the store can say "3 updates ready" and nothing
else, so "ready" covers everything from *the index mentions a newer digest* to
*every device I own is running it, compiled*. This is the record of what the
phases are, who owns each one, and what the surface may claim at any moment.

## Phases

| phase | question | owned by | where it is read from |
|---|---|---|---|
| `available` | does the index name a digest other than the installed one? | the store index | `soil-store plan` |
| `fetched` | is the tar in the package cache? | the host's package cache | `.packages/<digest>.spore` exists |
| `verified` | did the operator key and the publisher delegation accept it? | `Index::verify` + `for_entry` + the tar's detached signature | nothing new: an unverified tar is never installed |
| `installed` | is the live tree this digest? | `fungos.store.installed@1` (intent), `.install/<id>.json` (projection) | `soil-install status` |
| `warm` | is the module compiled in the host's AOT cache? | the host that will run it | a marker written when the module is precompiled |
| `everywhere` | do all the devices this package is for have it? | the fleet | per-node intent, via mycelium |

Progress is *phases complete over phases applicable*, not bytes. Bytes are a
detail of one phase; the person is waiting on the whole chain, and the chain is
short enough to show as steps.

`everywhere` is not applicable until the fleet can answer. It is skipped rather
than shown as perpetually pending, and the surface says which devices it counted
when it does apply. A package whose `[placement] devices` excludes this machine
is not a straggler here: it is not this machine's row at all.

## What "done" means

An update is done for a device when `installed` and `warm` are true for the
digest, and done overall when `everywhere` is true as well. Nothing else counts:
not a downloaded tar, not a queued job, not "all but the phone".

## Opening never blocks

A surface that waits for a fleet to converge is a surface nobody opens. The
store renders whatever phases are known, immediately, and prefers the local
truth it can read in a millisecond over a fleet answer that takes a second:

- unknown is rendered as unknown, never as failed and never as done;
- a straggler is a row that is complete locally and incomplete elsewhere;
- no request on the open path waits for a device to answer;
- the fleet view fills in behind the surface, and the row changes when it does.

This is the same rule as the catalog: read what is here, and let what is missing
be missing.

## Where the work goes

1. **The runner** - done. `soil-install update` walks each pending update through
`fetching -> fetched -> installed -> warm -> done`, journals every step to
`<root>/.updates/<id>.json`, and continues past a failure instead of stopping the
machine's whole queue. `soil-install progress --root DIR` reads the journal back.
Classes that need a person (`consent`, `system`) are staged, never applied.
2. **The warm step** - done. `soil-command warm` compiles each module into
wasmtime's cache and records the digest in `.warm/<name>.json`, so warm is
readable without compiling again. `done` requires it: with no runner the run
stops at `installed`.
3. **The surface.** The journal is what a storefront reads. What remains is the
path from the journal to the pixels: `soil-store serve` exposing it, and the
store Spore drawing a bar per row. Opening must not wait on it - the journal is a
file read, and a missing entry means unknown, not failed.
4. **The fleet.** `everywhere` needs two facts this tree does not have yet: the
account's devices (and their classes) and what each of them has installed.
`[placement] devices` already says which devices a package is *for*; the
inventory and the per-node intent are the principal and sync work.

Until 3 and 4 exist, the surface can still show `available`/`fetched`/
`installed`/`warm` - which is already more than "3 updates ready".
