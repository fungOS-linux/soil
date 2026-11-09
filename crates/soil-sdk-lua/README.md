# soil-sdk-lua

Generate a Lua API from the canonical Soil WIT contracts. The generator does not
implement any capability; it emits bindings to one host dispatcher so the same
WIT and the same host implementations remain the source of truth.

```sh
cargo run -p soil-sdk-lua -- --wit wit --out crates/soil-sdk-lua/generated
```

Both paths default to `wit` and `crates/soil-sdk-lua/generated`, resolved
against the current directory, so the command above and the bare
`cargo run -p soil-sdk-lua` behave the same when run from the repo root.

The checked-in output is:

- `generated/soil.lua` — the Lua module;
- `generated/soil-sdk.json` — signatures, resource metadata, docs and type
  shapes for the host side.

Run the stale-output gate from the repo root with:

```sh
cargo run -p soil-sdk-lua -- --check
```

## Host contract

The module reads a global `__soil_call` function:

```lua
__soil_call(interface_id, function_name, args, self)
```

- `interface_id` is the fully qualified WIT interface, for example
  `soil:http@0.1.0/http`.
- `function_name` is the original WIT name. Resource functions use
  `resource.method` (for example `store.delete`).
- `args` is always a table of named WIT parameters, using the original kebab-case
  names. It is `{}` when the function has no parameters.
- `self` is the opaque host handle for a resource method; it is `nil` for free
  functions, statics and constructors.

The SDK does not implement a transport, retry policy, credential lookup, or
capability check. Those stay in the host. A host that does not install
`__soil_call` fails loudly at the first WIT call.

## Lua shape

Paths are `M.<namespace>.<package>.<interface>.<function>`. When a package has
exactly one interface, the package table aliases it for convenience:

```lua
local soil = require("soil")

local response = soil.soil.http.fetch({
  url = "https://example.com/",
  method = "get",
  headers = {},
  max_bytes = 65536,
  timeout_ms = 5000,
})

local value = soil.soil.records.scan({
  collection = "elmers.run",
  limit = 50,
})
```

For a package with multiple interfaces the full path is required, for example
`soil.fungos.actions.actions.catalog({})`.

Function names are normalized to Lua identifiers (`compare-and-swap` becomes
`compare_and_swap`). Resource types use PascalCase and are exposed as tables, and
resource functions take the host handle as `self`:

```lua
-- method: dispatch("soil:appdata@1.0.0/appdata", "store.delete", args, self)
return store:delete({})
```

Statics and constructors follow the same table, called with `.` instead of `:`
(`Store.open({})`, `Store.new({})`). No interface in `wit/` declares one yet, so
the path is generated but not currently exercised against a real host.

Enums and flags are constants under `<interface>.types`:

```lua
if request.method == soil.soil.http.types.Method.GET then ... end
```

Record metadata is emitted as a table of field names and WIT type labels. The
host owns value validation; the generator does not synthesize a second schema
authority.

## Result convention

The dispatcher returns host-defined Lua values. The recommended mapping is the
one a host should use consistently:

- `option<T>`: the value or `nil`;
- `list<T>`: a Lua array;
- `result<T, E>`: `{ ok = value }` or `{ err = error }`;
- variants: `{ tag = "case", value = value }`;
- flags: a list of flag strings.

This is a binding convention, not a second WIT ABI. The Rust host remains
responsible for enforcing byte limits, grants and deadlines.
