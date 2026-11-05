# Soil operator and inference config

Mush resolves Operator's inference and MCP routes from one Soil-owned config so
provider settings are not scattered across shell exports and Operator overlays.

## Location

Resolution order:

```text
--config PATH
<vfs-root>/.soil/config.toml
~/.config/soil/config.toml
```

## Inference profile

```toml
[inference]
default = "umie"

[inference.profiles.umie]
provider = "unibus-umie"
service = "umie/inference"
services = "~/.cache/mycelium/services.json"
model = "laya"
credential = "geode://umie"

[inference.profiles.local]
provider = "openai-compatible"
base_url = "http://127.0.0.1:8096"
model = "laya"
api_key_env = "UMIE_TOKEN"
```

Resolution:

- `base_url` wins when present.
- Otherwise `service` is looked up in the host-granted `services.json` snapshot.
- A discovered endpoint gets `/v1` appended unless it already ends there.
- `api_key_env` reads a host environment variable.
- `credential = "geode://NAME"` resolves through the native host Geode adapter:
  a declared `geode get NAME` command. `GEODE_NAME` remains a development
  fallback. Resolved values are cached for the session and never enter
  Operator's environment.
- Operator receives a loopback `BIFROST_BASE_URL`, an ephemeral
  `BIFROST_API_KEY`, and `OPERATOR_MODEL`; it never receives the upstream key,
  service catalog path, or Geode handle.

## Operator and MCP

```toml
[operator]
operator_bin = "operator-tui"
model = "laya"

[operator.mcp]
url = "http://127.0.0.1:8792/mcp"
token_env = "UNIBUS_MCP_TOKEN"
```

This maps to Operator's MCP bridge:

```text
EDGERUNNER_MCP_URL
EDGERUNNER_MCP_TOKEN
```

Tools exposed by `unibus-mcp`/DCP therefore appear as `mcp__<server>__<tool>`
inside Operator. The endpoint is host-selected; Operator does not discover it.

## Geode adapter

```toml
[geode]
command = "/usr/local/bin/geode"
timeout_ms = 5000
```

The command contract is:

```sh
geode get NAME
```

It prints the secret to stdout. The host invokes it without a shell, bounds the
run, caches the result for the session, and never passes the value to Operator.
If no command is configured, `GEODE_NAME` is accepted as a local development
fallback.

## Loopback gateway

`mush run op` starts a loopback OpenAI-compatible gateway by default. It:

```text
accepts Operator's /v1 requests
validates the ephemeral session token
scrubs request JSON with soil-scrub
injects the resolved upstream credential
streams the upstream response, including SSE
```

The request is scrubbed before it leaves the host. The response is passed
through as a stream because the model only ever saw placeholders. Tool-result
scrubbing still belongs at the tool boundary.

Disable it explicitly:

```sh
mush run op --no-gateway
```

## Use

```sh
mush run op --inference-profile umie
mush run op --inference-profile local --prompt "fix tests"
mush run op --config ./soil.toml
```

Overrides:

```sh
mush run op --inference-profile umie --base-url http://127.0.0.1:8096 --model laya
```

## Boundaries

- Soil chooses the provider, endpoint, model alias, MCP route, and credential.
- Operator sees a base URL and an opaque gateway/session key, never Geode.
- The loopback gateway already provides request/response scrubbing and upstream
  credential injection. Token budgets, true SSE streaming, provider failover,
  and UMIE alias mapping are next.
- `unibus-mcp`/DCP is the tool transport; no host `PATH` discovery is required.
