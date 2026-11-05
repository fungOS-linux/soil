# Scrubber

Status: **first local reversible-redaction slice, 2026-11-05.**

The scrubber keeps sensitive values out of model prompts, tool output, and
session history while still letting an authorized host tool use the real value.

It is deliberately **not** a hash:

- a hash cannot be restored before exec
- an unsalted hash of a phone number or email is dictionary-attackable
- a stable global token links the same secret across sessions

Instead, each session gets a random key and a local token vault.

## Token shape

```text
⟦mush:email:c5895ef7⟧
⟦mush:phone:2b91ee70⟧
⟦mush:api_key:71c4a0f2⟧
⟦mush:secret:484d0f68⟧
⟦mush:credential:9ae2013f⟧
```

The identifier is a keyed BLAKE3 digest of the kind and value. The value is
stored only in the in-memory vault. The same value gets the same token inside
one `Scrubber`; a new session gets different tokens.

## Scopes

Every token carries restore scopes:

```rust
let token = scrubber.tokenize(
    Kind::Credential,
    "geode://github_token",
    &["http:api.github.com"],
);
```

```rust
let safe = scrubber.restore(&text, "bash");               // unchanged token
let real = scrubber.restore(&text, "http:api.github.com"); // value restored
```

Unauthorized tokens are left untouched, not turned into an error. A model
cannot force expansion by emitting a guessed token.

## Detectors

Current first-slice detectors:

```text
email addresses
phone-like numbers
known API-key prefixes (sk-, ghp_, github_pat_, xox*, AKIA, AIza, npm_, ...)
payment cards with a Luhn check
PEM/OpenSSH private-key blocks
KEY=value / TOKEN=value / SECRET=value assignments
```

The detector is local and best-effort. It does not call a remote NER service.

## CLI

```sh
$ echo 'email ada@example.com token=supersecretvalue' | soil-scrub
email ⟦mush:email:c5895ef7⟧ token=⟦mush:secret:484d0f68⟧
```

```sh
soil-scrub --text 'ada@example.com' --scope records.upsert
```

This is a one-shot debugging filter. A real gateway keeps a `Scrubber` alive
for the session.

## Library

```rust
use soil_scrub::{Kind, Scrubber};

let mut scrubber = Scrubber::new();
let prompt = scrubber.scrub("mail ada@example.com");
// -> "mail ⟦mush:email:...⟧"

let tool_input = scrubber.restore(&prompt, "records.upsert");
// -> "mail ada@example.com" only because the token allows that scope
```

For known values, bypass detection:

```rust
let token = scrubber.tokenize(Kind::ApiKey, value, &["http:api.example.com"]);
```

## Credentials and Geode

Credential values belong in Geode, not in the token vault.

```text
Geode            actual encrypted credential
Scrubber token   opaque reference usable by named tools
Mush env         token only
History          token only
Inference        token only
```

A credential token should be scoped to a tool and destination:

```text
⟦mush:credential:...⟧
  -> http.fetch, destination api.github.com
  -> git.push via host credential broker
```

Not:

```text
bash
vfs.write
arbitrary URL
unknown destination
```

For the first slice, `Scrubber::tokenize(Kind::Credential, ...)` can store a
Geode reference string such as `geode://github_token`. A future integration
should make that entry a native Geode handle rather than a URI string.

## Redaction boundaries

```text
prompt / tool result -> scrub -> model
model tool call       -> scope check -> restore -> host tool
host tool result      -> scrub -> model
```

The host never restores into an interactive shell environment. If the agent can
run `env` or `sh -c`, a real credential in that environment is already leaked.
Use structured tools, credential helpers, or a host proxy instead.

Do not silently restore tokens into VFS writes. That would put the secret into
shared content, the search index, or sync. Structured record writes may restore
one field for one authorized operation.

## Integration plan

1. Keep one scrubber per Mush/Operator session.
2. Redact provider requests and tool results.
3. Resolve Geode handles only inside the host tool bridge.
4. Record redacted command history by default.
5. Add a local provider proxy for Bifrost/OpenAI-compatible APIs.
6. Add policy for scopes, destination allowlists, uses, and TTL.

The scrubber is the reversible mapping primitive; the tool boundary is the
authorization boundary.
