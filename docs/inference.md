# `soil:inference`

Status: **first host contract, 2026-11-05.**

`soil:inference@0.1.0` is the host-mediated model interface for Spores. It is
not a provider SDK: endpoint discovery, Geode credentials, model aliases,
scrubbing, and transport stay in the host.

## Surface

```wit
models()                -> list<model>
complete(request)       -> response
open-stream(request)    -> completion-stream
state()                 -> inference-state
```

`completion-stream` is a polled resource:

```wit
resource completion-stream {
  next(timeout-ms) -> result<option<chunk>, error>;
  cancel()          -> result<_, error>;
}
```

A chunk carries raw provider bytes and a `done` flag. For OpenAI-compatible SSE
the host returns the event payload; the guest owns SSE/JSON interpretation.

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
- scrubs message content before the request leaves the host
- lists models from `/v1/models`
- implements `complete` as a non-streaming `/v1/chat/completions`
- implements `open-stream` as an SSE `completion-stream`

The guest does not see the endpoint, API key, Geode value, or service catalog.

## Boundaries

- `open-stream` returns raw chunks; the Spore owns parsing and presentation.
- `next` is bounded by the host chunk size; the current slice ignores the
  timeout hint after the upstream connection is open.
- Response content is not unredacted inside the inference host. Tool results
  and other user-visible values are handled at their own host boundaries.
- Streaming is HTTP/SSE for the first implementation. Other provider transports
  can implement the same resource without changing the contract.
