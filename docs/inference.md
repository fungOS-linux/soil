# `soil:inference`

Status: **first host contract, 2026-11-05.**

`soil:inference@0.2.0` is the host-mediated model interface for Spores. It is
not a provider SDK: endpoint discovery, Geode credentials, model aliases,
budgets, scrubbing, and transport stay in the host.

## Surface

```wit
models()              -> list<model>
complete(request)     -> response
open-stream(request)  -> completion-stream
state()               -> inference-state
```

`completion-stream` is a polled resource:

```wit
resource completion-stream {
  next(timeout-ms: u32) -> result<option<chunk>, error>;
  cancel()              -> result<_, error>;
}
```

A chunk carries raw provider bytes and a `done` flag. For OpenAI-compatible SSE
the host returns the event payload; the guest owns SSE/JSON interpretation.

## Requests

A request carries:

```text
model
messages
tools
max-output-tokens
temperature
prompt-cache-key
```

Messages preserve the full tool loop:

```wit
record message {
  role: system | user | assistant | tool,
  content: option<string>,
  name: option<string>,
  tool-call-id: option<string>,
  tool-calls: list<tool-call>,
  attachments: list<attachment>,
  reasoning: option<string>,
}
```

Assistant `tool-calls` and `tool` messages with `tool-call-id` are required for
Operator-style agent loops. Tools are OpenAI function definitions; the JSON
Schema arrives as a serialized string so the WIT does not encode provider
schema dialects.

Image attachments carry bytes plus an optional extracted-text field. The host
encodes them as provider content parts; it does not expose a file path.

`complete` returns the assistant message, finish reason, and usage. It can
return tool calls even without public text.

## Models

`models()` calls the resolved provider's `/v1/models` endpoint and returns:

```text
id
provider
context-window
```

The host supplies the provider label; the Spore never selects one.

## Host mapping

```text
Soil config profile
  -> base URL or Unibus/Mycelium service snapshot
  -> Geode credential
  -> host HTTP transport
  -> soil:inference
```

The `crates/soil-inference-host` implementation:

- loads the shared `SoilConfig`
- resolves `geode://` handles
- scrubs message content, tool arguments, tool descriptions, and attachment text
  before the request leaves the host
- lists models from `/v1/models`
- implements `complete` as a non-streaming `/v1/chat/completions`
- implements `open-stream` as an SSE `completion-stream`
- asks for usage in streamed requests

The guest does not see the endpoint, API key, Geode value, or service catalog.

## Operator

Operator has one provider type with two transports:

```text
native Operator         -> HTTP/Bifrost
Operator compiled as a Spore -> soil:inference
```

The choice is a compile-time target property, not an environment switch. An
Operator Spore links `soil:inference`; a native Operator keeps its existing
HTTP transport. Both paths share message conversion, SSE parsing, tool-call
accumulation, loop guards, and turn construction.

The Spore transport has no `prepare_session` and no embeddings. Those calls
return `unsupported` rather than falling back to a side channel. The host can
add them as separate capabilities later.

## Boundaries

- `open-stream` returns raw chunks; the Spore owns parsing and presentation.
- `next` is bounded by the host chunk size; the current slice ignores the
  timeout hint after the upstream connection is open.
- Response content is not unredacted inside the inference host. Tool results
  and other user-visible values are handled at their own host boundaries.
- Streaming is HTTP/SSE for the first implementation. Other provider transports
  can implement the same resource without changing the contract.
