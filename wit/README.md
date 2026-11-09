# Canonical WIT contracts

This directory contains Spore-facing contracts only. Transport, routing and
credential resolution stay in host adapters.

## Status

| Status | Meaning |
|---|---|
| wired | host implementation or real Spore proves the contract |
| contract | reviewed surface, host implementation still pending |
| deferred | reviewed idea, not exposed as an available capability |

| Package | Status | Notes |
|---|---|---|
| `soil:principal@0.1.0` | wired | host-attested Mycelium/OIDC principal; no token exposure |
| `soil:input@1.0.0` | wired | shared normalized input types; no grant |
| `soil:time@1.0.0` | wired | monotonic clock and host ticks |
| `soil:vfs@1.0.0` | wired | read-only workspace namespace |
| `soil:vfs-mut@0.1.0` | wired | separate mutation grant |
| `soil:index@1.1.0` | wired | derived VFS browse/search/KNN |
| `soil:records@0.1.0` | wired | durable schema-tagged records |
| `soil:search@0.5.0` | wired | global derived search + typed mention resolver |
| `soil:graph@0.1.0` | wired | read-only record links |
| `soil:http@0.1.0` | wired | bounded host-mediated HTTP |
| `soil:creds@1.0.0` | wired | host-held signing and verification; no secret export |
| `fungos:http-context@0.1.0` | wired | host-attested invocation context for `wasi:http` handlers |
| `soil:unibus@0.1.0` | wired | bounded poll/publish over Unibus; `soil-wasi-host` adapter |
| `soil:appdata@1.0.0` | contract | guest SDK exists; native host next |
| `soil:audio@0.1.0` | contract | room-aware PCM sink over `wasi:audio`; host resolver in `crates/soil-audio-host` |
| `soil:storage@1.0.0` | contract | content-addressed asset cache |
| `soil:media@0.1.0` | wired | playback, encoded chunks, image |
| `soil:surface-import@0.1.0` | wired | host-owned imported surfaces |
| `soil:inference@0.2.0` | wired | models, tools, completion stream |
| `soil:notify@1.1.0` | wired | inbox/focus/history/cleanup and ringer |
| `fungos:actions@1.0.0` | wired | action bus plus presentation catalog |
| `soil:ble@1.0.0` | wired | BLE host and COLMI ring Spore |
| `soil:canvas@1.0.0` | wired | owner surface, `soil-scene` document, guest SDK and component host |
| `soil:terminal@0.1.0` | contract | host-rendered terminal surface |
| `soil:view@1.0.0` | contract | host-reviewed web/Spore/retained view |
| `soil:mycelium@0.1.0` | deferred | directory/session; no host yet |
| `soil:device@0.1.0` | wired | battery/estimate/lid core-Wasm lowering |
| `wasi:audio@0.1.0` | wired | draft cpal audio sink |

## Not Spore contracts

- `soil:unibus` (above) is the Spore-facing poll/publish edge over the Unibus
  transport. It supersedes `fungos:events`, which was removed: there is one
  publication surface, not two.
- `soil:ringer` was merged into `soil:notify` as the `ringer` interface.
- `soil:canvas/actions` was merged into `fungos:actions/catalog`.
- CRM, mail, contacts, projects, tasks and calls are records schemas, not WIT.
- Espo import and other provider integrations are native host adapters.

## Core-Wasm profile

`soil:device` and the Canvas surface are semantic WIT, but the S22 runtime links
them through a bounded core-Wasm ABI. See
[`docs/abi-core-wasm.md`](../docs/abi-core-wasm.md).

## Wire protocols

No contract here is an RPC protocol. Records sync, notifications, rings and
actions use schema-tagged Unibus envelopes. Provider integration uses
HTTP/OpenAPI. gRPC is only for a future cross-language native service.
