# `soil:audio` — rooms, not devices

Status: **contract + host resolver, 2026-11-08.** `crates/soil-audio-host`
implements the WIT over the existing `wasi:audio` sink; the static room table
and its tests run. Dynamic presence and the routing control plane are the next
host adapter.

## The gap

`wasi:audio@0.1.0` is a portable PCM sink:

```wit
open: func(format: format) -> result<output, error>
```

One host, one host-selected sink. That is the right portable contract, but it
cannot express the thing a desktop actually needs: *hear this where I am*.
The principal may be at the studio desk, on the S22 with IEMs, or in the lab;
the same Spore should not have to know which.

`soil:audio` adds a **room** in front of the sink:

```wit
open: func(format: format, room: option<string>) -> result<output, error>;
rooms: func() -> result<list<room>, error>;
current-room: func() -> result<option<room>, error>;
```

A room is a host-resolved **name**: `home-pi`, `lab`, `local`, `current`. It is
never an address, port, SSRC, device path, or socket. The guest writes PCM; the
host decides which receiver or device carries it.

## Resolution

| Request | Host behaviour |
|---|---|
| `none` | the host default; currently the same as `current` |
| `current` | the room the principal is in; `not-ready` if the host has none |
| a name | that room; `unknown-room` if it is not in the table |
| a room that is not `live` | `no-listener` |

`no-listener` is deliberate. A room that exists but has no attached receiver is
the tree falling in the forest; the board should say so, not play into the void
or silently fall back to the laptop speaker. `live` is the host's belief that a
receiver is attached *now*.

## Host shape

[`crates/soil-audio-host`](../crates/soil-audio-host) exposes a `RoomDirectory`:

```rust
pub trait RoomDirectory: Send + Sync {
    fn rooms(&self) -> Vec<Room>;
    fn current(&self) -> Option<String>;
}
```

`StaticRooms` is the first implementation: an operator config, and a `local`
room that always works. A presence adapter implements the same trait from the
facts the host already has — which device is attached, which Isochrone route is
connected — without changing the WIT or any Spore.

Isochrone stays an **application/service**, not a host capability, exactly as
[`abi-map.md`](abi-map.md) says. A room's `RoomTarget` may be an Isochrone
destination today; nothing in `soil:audio` names Isochrone.

## Relationship to `wasi:audio`

`soil:audio` is a routing facade over the same sink implementation. A Spore that
only needs the local device keeps importing `wasi:audio`; a Spore that needs a
room imports `soil:audio`. The host links one or the other, not both for the
same output.

## Limits

- **One output per open.** Overlap is the host mixer's job; the cpal sink today
  opens one ring per output, and the Isochrone sink sends one stream.
- **Isochrone is not paced by this layer.** The sink accepts PCM as fast as it
  is written; short SFX are fine, a long stream needs a pacer or a receiver
  that rebuffers. See [`wasi-audio.md`](wasi-audio.md).
- **Liveness is a host belief.** `StaticRooms` is config; a presence adapter is
  the next step. A stale `live = true` plays into a silent room; a stale
  `live = false` refuses a room that is actually listening.
- **No capture.** `soil:mic` stays separate so muting output never grants a
  microphone.
- **No per-Spore quota yet.** The sink bounds a call and a queue; a noisy Spore
  and a quiet one are not separated.

## Tests

```sh
cargo test -p soil-audio-host
```

The resolver tests cover current/named/unknown/silent/no-current and the
local-only host. They do not open a device; the audio-device proof stays the
ignored `wasi-audio-wasmtime` tone test.
