# Viewer Spore

Status: contract and core state machine, 2026-10-27. This is the first
concrete Soil workload, not a shipped app. The canonical WIT contract is
[`wit/soil-view.wit`](../wit/soil-view.wit); the pure state machine is
[`crates/viewer-spore`](../crates/viewer-spore). Host and Fire TV adapters are
still pending.

## Goal

One viewer that presents the same slide/deck source on a laptop, the lab Fire TV
and future fungOS screens, with real remote navigation and bounded capture. It is
not another video stream: the viewer owns presentation state, the host owns the
display, browser/Spore content and capture.

The first content target is the existing FPL venture deck (`?view=present&
receiver=1`). The same contract should later present allowlisted web pages,
signed Spores and native retained scenes.

## Split of responsibility

| Layer | Owns | Never owns |
|---|---|---|
| Viewer Spore | deck manifest, slide index, chrome, remote action mapping, presentation state | a browser engine, raw URLs, display handles, credentials, screen capture buffers |
| Soil host | surface/window, input normalization, focus, fullscreen, content target resolution, capture readback | the deck's slide semantics or authoring data |
| Canvas/Fire TV | physical display, remote key events, hardware decode/GL, user-visible capture indicator | deck state or target allowlists |
| Spore Store / Mycelium | signed manifest, CID resolution, capability grants, revocation | runtime authority or a second ACL |

Transport stays in Soil. The viewer does not open sockets, read files or receive
a WebView/display pointer. Web and WASM content arrive through a host capability
that names a reviewed target, never a URL chosen by the Spore.

## Proposed capability: `soil:view@1`

The canonical package is [`wit/soil-view.wit`](../wit/soil-view.wit). The
sketch below is the same contract in review form.

A host-owned content view. The Spore controls navigation and presentation; the
host owns the engine and pixels.

```wit
// One view per Viewer Spore. The target is an id in the host's reviewed index,
// never an arbitrary URL, path or CID supplied by wasm.
package soil:view@1.0.0;

interface viewer {
  enum kind { web, spore, retained }

  record target {
    id: string,
    kind: kind,
    title: string,
  }

  record state {
    target: target,
    position: u64,       // slide/page/step, zero-based
    count: u64,
    loading: bool,
    fullscreen: bool,
    generation: u64,
  }

  enum action {
    next,
    previous,
    first,
    last,
    select,
    back,
    fullscreen-on,
    fullscreen-off,
    fullscreen-toggle,
  }

  enum error {
    denied,
    unknown-target,
    not-ready,
    unsupported,
    over-budget,
    capture-denied,
    invalid,
    closed,
  }

  resource view {
    act: func(action: action) -> result<state, error>;
    observe: func(timeout-ms: u32) -> result<state, error>;
    capture: func(max-bytes: u32) -> result<list<u8>, error>; // PNG
    close: func() -> result<_, error>;
  }

  open: func(target: string) -> result<view, error>;
}
```

Rules:

- `open` resolves only a target id already present in the host's signed/reviewed
  index. Redirects and new origins are refused.
- `capture` is a separate grant (`soil:view.capture`) and always shows a visible
  host indicator. It returns one bounded PNG, never a live frame callback.
- No callbacks into wasm. `observe` is polled with a deadline.
- `state.generation` increases on target/navigation/loading changes so a late
  observation cannot move the viewer backwards.
- Unsupported content is `unsupported`, never an empty success.

The viewer also imports:

- `soil:surface@1` for its own retained controls/chrome and focused D-pad/OK/back.
- `soil:input@1` for shared normalized key/modifier types only. It has no global
  input grant; media keys arrive through the focused Canvas surface or are
  mapped by the host directly into `soil:view` actions.
- `soil:storage@1` for the deck manifest and content-addressed assets.
- `soil:time@1` for bounded polling and transitions.

It does **not** import `soil:net`, `soil:api`, `soil:process` or
`soil:secrets`. A web target is rendered by the host's browser engine; a Spore
target is launched by the host in its own instance. The viewer never hosts a
nested wasm runtime.

## Content modes

### Web

The host runs the reviewed page in its browser engine (Canvas browser host,
WebView or a future Servo adapter) and exposes only the view actions. The deck
uses its existing receiver mode, so authoring chrome is absent. The host remaps
Fire TV keys; the page receives normalized next/previous/fullscreen commands.

### Signed Spore

The target id resolves to a signed Spore manifest in the Store. The host starts
that Spore in a separate capability boundary and gives the viewer only the
bounded `soil:view` surface. A viewer cannot grant another Spore authority or
read its memory.

### Retained scene

The host composites a native retained scene (deck renderer, PDF or static
asset) behind the same view handle. This is the path for the eventual all-Soil
renderer and for content that should not carry a browser.

## Fire TV remote map

Raw Android keycodes and media keys stay in the host. The viewer receives named
actions only.

| Remote | Viewer action |
|---|---|
| D-pad left | previous |
| D-pad right | next |
| D-pad up/down | move focus within viewer chrome |
| OK / Enter | select focused control; next when content has focus |
| Back | close menu/exit fullscreen; never leave a presenting deck silently |
| Play/Pause | fullscreen toggle |
| Rewind | previous |
| Fast-forward | next |
| Menu / Info | viewer status or slide overview |

Fullscreen means the host surface fills the display and the host hides its
workspace chrome. The viewer still owns the slide index. A radio or system key
that the host does not map is not forwarded.

## Capture

Capture is a host operation, not a screenshot of wasm memory:

1. The viewer requests `capture(max-bytes)`.
2. The host checks the `soil:view.capture` grant and current target generation.
3. The host reads the presented surface/WebView after the next frame, encodes
   one bounded PNG and returns it.
4. The capture indicator stays visible for the operation. Background or repeated
   capture is refused.

On Fire TV the host can use the platform GPU/PixelCopy path. The viewer must not
receive the raw bitmap or a capture ring buffer.

## Lifecycle and failure

- Opening a target creates one host view and one viewer generation.
- Switching workspace may detach the view without deleting the deck index; the
  host decides whether the target stays warm.
- Closing the viewer closes the host view and revokes pending capture.
- If the host view crashes, the viewer reports `not-ready`; it does not silently
  fall back to a stale HLS stream.
- A rejected manifest or target fails with a named error. The last good scene is
  retained only by the host, not by a second viewer copy.
- The existing Canvas/Surf HLS path remains a fallback until a Soil viewer passes
  real Fire TV acceptance.

## Staged implementation

1. **Contract and fixtures (this repo).** Keep this document, add WIT/conformance
   fixtures when the runtime crate moves here, and publish a signed viewer Spore
   manifest with only the grants above.
2. **Canvas Android host adapter.** Add a host-owned web-view surface and remote
   action map. Prove navigation, fullscreen, back behavior and capture on the lab
   Fire TV without HLS.
3. **Target index and capture grant.** Resolve `web`, `spore` and `retained`
   targets from a reviewed index; implement bounded PNG capture with a visible
   indicator.
4. **Soil runtime port.** Run the same viewer Spore against the extracted Soil
   runtime on macOS/Linux/Android. Keep Canvas as one host, not the definition of
   the viewer.

## Acceptance

- Fire TV: D-pad left/right changes slides with no laptop or Surf controller.
- Fire TV: Play/Pause enters/exits fullscreen; Back exits fullscreen before
  leaving the deck.
- The visible slide index survives a workspace switch and a host view reload.
- Capture returns one PNG matching the displayed slide and is refused without the
  capture grant.
- No external network request, remote font, credential or arbitrary URL is
  reachable from the viewer Spore.
- A missing, revoked or mismatched target fails loudly; it never shows a stale
  slide as current.

## Open decisions

- Whether capture belongs to `soil:view` or a separate `soil:screen` capability.
- Whether the host browser is Servo, WebView or the Canvas browser host on each
  target.
- Whether the Fire TV shell keeps a persistent viewer Spore or starts one per
  workspace activation.
