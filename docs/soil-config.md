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
- `credential = "geode://NAME"` currently resolves through `GEODE_NAME`; the
  native Geode resolver will replace this bridge.
- Operator receives `BIFROST_BASE_URL`, `BIFROST_API_KEY`, and
  `OPERATOR_MODEL`; it never receives the service catalog path or Geode handle.

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
- A future loopback inference gateway can add request redaction, token budgets,
  provider failover, and UMIE model alias mapping without changing Operator.
- `unibus-mcp`/DCP is the tool transport; no host `PATH` discovery is required.
