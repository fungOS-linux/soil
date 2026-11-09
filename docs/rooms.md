# Rooms: shared audio/video spaces

Status: **design, presentation fixture and first service slice, 2026-11-09.**
`unibus-room` implements room membership, presence and bus declarations as a
Unibus hub plugin
([runbook](https://github.com/fungOS-linux/unibus/blob/main/docs/room-service.md)).
`soil:room`, the media relay, screen share and the Holodeck adapter are still
unimplemented.
[`spores/rooms`](https://github.com/fungOS-linux/spores/tree/main/spores/rooms)
renders the snapshot in this note against a fixture, so the shape can be
reviewed before the host contract is committed.

A room is one shared space with two presenters. **Rooms** is the flat
Canvas presenter: audio, video, text and screen share. **Holodeck** is the VR
presenter of the same space. They share a `space` id, a member list and a media
plane; only the renderer differs. An office is a workspace of these rooms.

## The shape

```text
workspace (Mycelium installation or team)
  room      voice | text | stage, and a shared space id
    member  human | agent | bot
    bus     speech (interactive) | program (buffered, music)
    thread  fungos.conversation messages
    screens host-owned surfaces, expiring
```

Two presenters, one membership authority:

```text
 Rooms (Canvas)                  Holodeck (LÖVR)
 flat A/V, text, screens         VR space, spatial audio, portals
        \                          /
         +---- room service (membership, presence, buses, signaling)
                                 |
                         media relay / edge
                 speech + program buses      bridges (deferred)
```

The Spore is a directory and presence surface. It is not a media engine. Voice,
music, screen share and the Holodeck handoff all terminate in host-side adapters
that the Spore never addresses directly.

## Why a Spore cannot own the room

The host contract is explicit: a Spore owns document semantics and a revisioned
snapshot, Canvas owns every pixel, and Soil owns grants and module lifecycle. A
Spore has no ambient sockets, clock, devices or media handles; it has exactly
the grants in its manifest. A "WebRTC Spore" is therefore not a peer connection
in Wasm; it is a snapshot of room state plus opaque handles the host resolves.

That is not a limitation to work around. It is what keeps room keys, SDP, SSRCs,
addresses and bridge tokens out of executable content that a user installs from
a store.

Two rules follow, and they match `soil:records` and `soil:http`:

- the host owns transport, peer connections, devices and credentials;
- the Spore names rooms, spaces, members, actions and surface aliases, never
  endpoints.

## Reuse map

| Need | Existing owner | What it proves | Keep |
|---|---|---|---|
| Room membership, poses, permissions | [`holodeck-server`](https://github.com/ajmwagar/holodeck/tree/main/server) and [protocol v1](https://github.com/ajmwagar/holodeck/blob/main/docs/multiplayer-server.md) | A deterministic room state machine: join/leave, peer list, last-writer-wins state, connection-scoped owner policy | The semantic model and the VR presenter, not the ENet server |
| Voice signaling inside a room | [`room-plugin/src/voice.rs`](https://github.com/ajmwagar/holodeck/blob/main/room-plugin/src/voice.rs) | SDP offer/answer relayed on the room channel; 640-byte PCM16 frames on an unordered WebRTC data channel | The proof that signaling and media are separate paths |
| Native audio transport | [`isochrone`](https://github.com/ajmwagar/isochrone) | RTP/L16/L24, timestamp playout, ASRC clock correction, peer-pinned streams | The LAN transport for both audio buses |
| Speech entering a room | [`JARVIS` `isochrone_input.rs`](https://github.com/FuturePresentLabs/JARVIS/blob/master/src/sound/isochrone_input.rs), [Holodeck `microphone-plugin`](https://github.com/ajmwagar/holodeck/tree/main/microphone-plugin) | 16 kHz mono L16, 20 ms packets, pinned peer IP and SSRC, no audio on the room bus | The exact `speech` bus format |
| Voice agent | JARVIS Sound | VAD, wake, STT, ordered TTS, cancellation epochs | The agent, not a second assistant |
| Browser voice | [JARVIS browser design](https://github.com/FuturePresentLabs/JARVIS/blob/master/browser-node-design.md) | Session authority is implemented; gateway and media are planned | One WebRTC edge, shared, not two |
| Text | [`soil-conversation`](../crates/soil-conversation), [`docs/conversation.md`](conversation.md) | Threads and messages with `external-id`, participants and `source_ref` provenance | Room text, and future bridges |
| Shared surfaces | [`soil:surface-import`](surface-import.md) | Host-owned surface, damage events, normalized input, no pixels in the guest | Screen-share consumption |
| Identity and admission | [`soil:principal`](../wit/soil-principal.wit) | `human`/`agent`/`service`, teams, host-mapped roles | Member roles |
| Spore-facing bus | [`soil:unibus`](../wit/soil-unibus.wit) | Bounded subscribe/publish by message kind | Interim transport while `soil:room` does not exist |

Holodeck's room voice already states the privacy rule this design keeps: *No
audio crosses the room peer bus.* Speech is a direct stream between the
participants that need it, not a property of room membership. The same rule
keeps a music bot's output off the speech bus.

## Deliverable 1: room service, `soil:room`, and the Rooms Spore

### Room service

Accepted: **`unibus-room`**, a self-hosted service in the Unibus repository,
because a room is an application session on `unibus/peer/1`, not a Mycelium
control-plane concern. Mycelium remains the identity and network-admission
source. The service owns:

- workspace and room directory, where each room has a stable `space` id shared
  with the VR presenter;
- membership and roles, projected from a Mycelium principal; v1 roles are
  owner, member and agent, with no custom role editor;
- presence: connected, idle, speaking, mic, share, hand, active presenters;
- audio bus declarations and per-bus publishers;
- text events that point at `fungos.conversation` records, never at bodies;
- media signaling and route allocation, opaque to Spores;
- the addressed-turn event that an agent acts on (below).

It owns no media bytes. It does not mix, record or decode audio.

Message kinds are versioned like every other Unibus application. The first
slice implements membership, presence, the directory and the bus snapshot:

```text
room.join.v1        room.leave.v1       room.self.v1
room.list.v1        room.welcome.v1     room.member.v1
room.list.reply.v1  room.error.v1

planned: room.signal.v1   room.share.v1   room.addressed.v1
```

`room.self.v1` carries `speaking` today. A separate high-rate speaking kind is a
later optimization, not a second source of truth. Implemented bodies and limits
live in the [Unibus room runbook](https://github.com/fungOS-linux/unibus/blob/main/docs/room-service.md).

No silent downgrade: a room hosted for a remote team requires an authenticated
`unibus/peer/1` session with a Mycelium grant. A LAN-only room may admit by
network, like the Holodeck importer does today, but it must say so and must not
claim authenticated identities.

### Who hosts a room

A room is hosted by a member's node, not by a global service. The first slice
is a hub plugin: a member runs the `unibus-room` binary, which is a Unibus hub
with the plugin installed. There is no separate room daemon and no central
registry. Consequences:

- there is no central room registry; the Mycelium directory and grants decide
  who may reach the host;
- when the host leaves, the room ends in v1; migration is a later design;
- media is relayed by the host by default, but peers that can reach each other
  may carry isochrone directly and use the host only to allocate the route;
- the `space` id is stable for the room's lifetime and is what the Holodeck
  presenter opens; it is not a global identifier.

### Where the peer connection lives

The peer connection is host-shell work, not module work. On `canvas-web` the
host page owns `getUserMedia`, the `RTCPeerConnection`, the audio worklet and
playback; on native, `soil-runtime` owns the equivalent. The Spore receives
presence, speaking state, bus state and surface aliases.

A host without media still renders the room: voice shows as unavailable by
name. It does not silently fall back to a text-only room that looks joined.

### `soil:room`

The Spore-facing capability. It is not written yet; the Rooms fixture and the
room service should settle the verbs first. The rules are already fixed:

- opaque room, space and member ids; no address, SSRC, SDP, token or key;
- no media bytes, samples, pixels or texture handles;
- a revisioned snapshot plus bounded events, polled with a timeout;
- actions are intents; the host decides whether the caller may act;
- media appears only as an opaque alias resolved by the host
  (`soil:surface-import` semantics);
- a bus is named by class and state, never by a codec or an endpoint.

Interim path: a prototype may use `soil:unibus` directly for `room.*` kinds.
That is a prototype escape hatch, not the ABI. Once two clients and one agent
agree on the semantics, the frames move behind `soil:room` so the protocol stops
being guest-visible.

### Rooms Spore

`spores/rooms` is the flat presenter: one flat snapshot in, one Lua scene out,
no authority. It renders the room list with VR counts, participant tiles with
speaking, agent and bot state, the audio buses, now playing, a screen grid where
each screen can go fullscreen, the conversation and the composer. The same file
renders a compact room widget (stats, now playing, screen summary and open
controls) and the focused-screen presentation.

```lua
{
  revision = 42,
  view = { mode = "room", focus = nil },
  workspace = { name = "FPL Rooms", presence = "connected" },
  self = { member = "person:avery", name = "Avery", room = "studio", mic = "on" },
  rooms = {
    { id = "studio", name = "Studio", kind = "voice", members = 4, unread = 2, vr = 2 },
  },
  room = {
    id = "studio", name = "Studio", topic = "Transmog + Quest", joined = true,
    space = "studio",
    buses = {
      { class = "speech", buffer_ms = 40, publishers = 3 },
      { class = "program", buffer_ms = 250, publishers = 1, lossless = true,
        now_playing = { title = "Voyager", artist = "Daft Punk" } },
    },
    members = {
      { id = "person:james", name = "James", kind = "human", speaking = true,
        mic = "on", presenter = "holodeck" },
      { id = "agent:jarvis", name = "JARVIS", kind = "agent", agent_state = "addressed" },
      { id = "bot:hifi", name = "Hi-Fi", kind = "bot", audio = "program", buffered = true },
    },
    screens = {
      { id = "share-current", owner = "person:james", title = "Transmog / fixie",
        kind = "window", surface = "share:current" },
      { id = "quest-camera", owner = "person:avery", title = "Quest capture",
        kind = "camera", surface = "quest:camera" },
    },
    messages = { { id = "m1", from = "James", text = "Rotation is live.", mine = false } },
  },
}
```

`surface` is a host alias, not a URL. `space` is the shared identity of the
room in both presenters. `agent_state = "addressed"` is how an agent states that
it is not always listening. `view` selects the presentation: `mode = "widget"`
for the compact card, or `focus = <screen id>` to fill the view with one screen.
Fullscreen is snapshot state; the view emits `fullscreen-screen` /
`exit-fullscreen` and the host moves `focus`.

```text
+--------------------------------------------------------------------------+
| FPL Rooms   ● connected   you: Avery                                     |
+------------------------+-------------------------------------------------+
| ROOMS                  | Transmog + Quest       joined · 4 · 2 in Holodeck|
| ● Studio   4 · 2 VR    | [Open in Holodeck]                              |
|   [Here]               | In room 4 | Speaking 1 | Speech 3 | Program 1      |
| ● Workshop 1           | [●] Avery   [●] James  [●] JARVIS   [●] Hi-Fi   |
|   [Join]               | speaking    active     addressed    playing    |
| ● Listening room 2     | AUDIO                                           |
|   [Join]               | [speech 40 ms · 3]   [program 250 ms · lossless]|
| [New room]             | ♪ Voyager — Daft Punk                           |
|                        | SCREENS                                         |
|                        | [Transmog / fixie]    [Quest capture]           |
|                        | [Fullscreen]          [Fullscreen]              |
|                        | CONVERSATION                                    |
|                        | James   Rotation is live on the table.          |
|                        | [Message the room]  [Ask JARVIS]  [Share]       |
+------------------------+-------------------------------------------------+
```

The widget reuses the same counts as a compact card. A focused screen replaces
the room with one `ui.host_surface` and a `Back to room` control; the host owns
the transition.

### One room, two presenters

The `space` id is the contract between presenters:

- Rooms joins a space and gets the flat snapshot above.
- Holodeck joins the same space and gets the same members, buses and screens.
  Its existing room server already models membership, poses and connection
  policy; the adapter replaces that front-end with `room.*` kinds while keeping
  the in-headset room semantics and portals.
- `Open in Holodeck` sends `{ action = "enter-holodeck", body = { space = <id> } }`.
  The host launches the VR presenter with that space, exactly like the existing
  back-wall portal opens a room. The Spore does not launch a process.
- A member has one presence but may switch presenters. Joining VR does not
  create a second membership, and leaving one presenter does not drop the
  other.

Spatial audio is a Holodeck concern: it positions the same `speech` bus streams
by avatar pose. The media plane does not know which presenter is listening.

### Screen share

Screen share is the one case that needs a new host capability, and it should be
split in two:

- **publish**: the host captures a chosen surface (one window or one Spore
  surface, not the whole desktop), encodes bounded damage frames, and publishes
  them to the room. The Spore sends an intent; the user picks the surface in a
  host-owned picker. A member may share one surface; the room owner may revoke
  it.
- **consume**: the existing `soil:surface-import` path. The Spore places each
  alias from `room.screens[]`; the host owns decode, texture and input
  forwarding. `view.focus` picks one alias for the fullscreen placement.

The publish side is `soil:screencast`, the mirror of `soil:surface-import`: the
host shows its own picker, captures and encodes a surface it already owns, and
returns an opaque session plus a surface alias. The Spore never receives pixels,
a display id or a captured window handle. On `canvas-web` the host shell uses
`getDisplayMedia`; on native a platform capture adapter (ScreenCaptureKit,
Wayland portal) does the same job. The contract is in
[`capture.md`](capture.md).

The Rooms Spore does not request `soil:mic`, `soil:camera` or `soil:screencast`.
The host room adapter uses those capabilities internally and projects only
member state and surface aliases into the snapshot. That keeps the permission
prompt, encoded buffers and media transport out of Lua.

Codec, frame rate and bitrate are a spike, not a decision. Damage frames bounded
to 15 fps and 2 Mbit/s are the target; full-motion video is a later adapter.
The existing privacy statement still applies: anything visible can be recorded
by the viewer.

## Deliverable 2: two audio classes

One audio format cannot serve both a conversation and a music bot. The room
carries two buses, and a participant may publish to either or both.

| | `speech` | `program` |
|---|---|---|
| Use | human microphones, JARVIS | music bots, DJ sets, shared audio |
| Format | 16 kHz mono L16, 20 ms packets | 48 kHz stereo; L24 lossless on LAN, Opus 256–320 kbit/s remote |
| Playout target | 40 ms | 150–1000 ms, publisher-buffered |
| Clock handling | isochrone playout buffer | isochrone playout buffer plus ASRC; a larger target absorbs drift and network jitter |
| Who listens | humans, agents | humans; agents only when asked |

Why separate buses:

- a music bot never forces a 2.3 Mbit/s L24 stream into the speech bus, and a
  human mic never degrades music;
- the relay can apply different buffers, bitrates and loss policies per class;
- JARVIS subscribes to `speech` only, so it does not transcribe the music;
- the UI can state what it is actually doing (`40 ms` vs `250 ms`, lossless vs
  lossy) instead of implying one quality for everything.

Buffering is a feature, not latency debt. A music bot can queue and start at a
planned time; a listener can tolerate a quarter second of delay to keep lossless
LAN audio intact. The publisher declares its buffer target, the room advertises
it, and the UI shows it. A program publisher that cannot hold its buffer stops
visibly rather than degrading the speech bus.

Lossless is a LAN capability, not an internet promise. Within a trusted LAN,
isochrone carries L24 over UDP and the existing ASRC absorbs clock drift;
remote listeners get a lossy, buffered Opus stream from the WebRTC edge. The
room service allocates the route by class; it never re-encodes.

Music bots are ordinary participants of `kind = bot` that publish `program` and
expose `now_playing` metadata. They are not a special path in the room service.

## Deliverable 3: JARVIS as a room participant

JARVIS is a member with `kind = agent`, not a bridge and not a second stack.

**Addressing is the admission path.** A room gives JARVIS the one thing a
native wake word does not need: an explicit `room.addressed.v1` event naming the
speaker's turn. The agent subscribes to that speaker's `speech` stream for that
turn, runs the existing STT/LLM/TTS path, and publishes its reply. It never
subscribes to the whole room by default, and never to `program` unless asked.
That keeps the existing rule that background speech is not admitted, and it
avoids a second always-on microphone.

Addressing sources, in order of preference:

1. a text message that mentions the agent in the room thread;
2. an explicit UI action (`Ask JARVIS`) on the speaker's tile;
3. the wake word inside audio the agent is already receiving for an open turn.

Media in: one isochrone subscription, 16 kHz mono L16, the same `StreamFormat`
that [`isochrone_input.rs`](https://github.com/FuturePresentLabs/JARVIS/blob/master/src/sound/isochrone_input.rs)
already pins. Media out: the existing ordered TTS queue, published to `speech`
with its own SSRC.

The missing JARVIS seam is headless capture and playback: today the live loop
owns CPAL devices. The browser design already requires the same seam
(`jarvis-8g9.4`), so room participation and browser participation should share
it rather than growing two.

## Deferred: literal Discord

Deferred by decision on 2026-11-09. No `discord-soil` crate, no bot, no voice
bridge in the current plan.

The shape is still the right one when it is picked up: a bridge is a room
participant that speaks two protocols, behind the same `RoomPort`/`MediaPort`
seams, with a text path through `fungos.conversation` and a voice path through
`songbird` 0.6+.

Facts to keep for that day:

- Discord requires DAVE (MLS-based end-to-end encryption) for all voice and Go
  Live audio since **2026-03-01**. A client that cannot negotiate it will not be
  able to send media. `songbird` 0.6.0 (2026-04-05) added DAVE support; there is
  no reason to implement MLS in this repository.
- `libdave` is Discord's C++/JS implementation and the protocol whitepaper is
  public.
- A bridge is a call participant with keys. E2EE ends at the bridge; room
  members behind it are not MLS members, and the UI must say so.
- Text normalizes into `fungos.conversation` with `external-id` and `source-ref`
  provenance. A Discord author is `discord:<id>`, not a Mycelium principal until
  an explicit enrollment link exists.

## Phases and exit criteria

| Phase | Work | Exit |
|---|---|---|
| 0 | This note and `spores/rooms` | `lua tests/rooms_fixture.lua` from `spores/rooms` passes; the snapshot is reviewed |
| 1 | `unibus-room` skeleton: rooms, `space` ids, members, presence, `room.*` kinds, a native probe | Landed in `crates/unibus-room` with a two-client loopback test; a real two-machine run is still pending |
| 2 | `speech` bus relay: 16 kHz mono, speaking state, bounded fan-out | Four humans talk in one room with no bridge |
| 3 | JARVIS addressed-only participant | JARVIS answers only when addressed, on `speech` |
| 4 | `program` bus: buffered publishers, lossless LAN, Opus remote, `now_playing` | A music bot plays losslessly on the LAN while speech stays at 40 ms |
| 5 | `soil:screencast` and the share tile | One window shared, bounded fps/bitrate, consumers use `ui.host_surface` |
| 6 | Holodeck room adapter | Holodeck joins the same space and sees the same members and buses |
| 7 | WebRTC edge and browser client | Browser joins a room; remote listeners get the buffered program stream |

Native first because it is the cheapest proof of the semantics: isochrone and
the speech format already work, and no ICE or TURN is involved. The Holodeck
twin is a presenter adapter, not a second room model; the `space` id lands in
phase 1 so the adapter can follow whenever it is scheduled.

Phase 1's service slice has landed: `crates/unibus-room` holds the contract, the
pure room state machine and the hub plugin; its integration test connects two
clients through a real loopback hub and checks join, presence, presenter switch,
directory and leave. The remaining exit is a real run between two machines and
the first media relay.

## Decisions

1. **Room service owner.** Accepted and landed as a hub plugin in
   `crates/unibus-room` over `unibus/peer/1` node sessions. Mycelium stays
   identity and admission. Extending `holodeck-server` is not recommended: it is
   an ENet/LAN front-end for LÖVR, not an authenticated multi-client service.
2. **First human client.** Accepted: Rooms, the Canvas Spore. Holodeck is the VR
   presenter of the same room, not a separate product.
3. **v1 audio.** Accepted: the `speech` bus only, 16 kHz mono L16. The
   `program` bus follows in phase 4 with publisher buffering, lossless on LAN
   and lossy remote; music is not forced through the speech path.
4. **Literal Discord.** Deferred. The bridge seams stay documented; no crate or
   bot work is scheduled.
5. **External identity.** Deferred with Discord. A Discord member is not a
   Mycelium principal until an explicit link exists.

## Non-goals and known gaps

- No mixing, recording or E2EE claim in v1. Native audio is relayed fan-out, not
  mixed; the browser edge terminates one WebRTC peer per browser, not a general
  SFU.
- No always-on agent listening. Addressing is the admission path.
- No whole-desktop capture in v1. One chosen surface, host-owned picker.
- No lossless guarantee off the LAN. Lossless is a route property, not a room
  property.
- No authenticated remote room until a room-join grant shape exists in Mycelium;
  this note does not define one. The LAN path must be labelled unauthenticated.
- Music-bot buffer sizing, LAN bandwidth for L24 and Quest decode are
  unmeasured. Phase 4 is where those numbers become evidence, not before.
