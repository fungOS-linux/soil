# soil:creds

Host-mediated credentials. A Spore names a credential; the host decides whether
the name is granted, and returns an opaque handle. **Secret bytes never enter
the guest**, and never enter a record, a log, or a unit file.

This is deliberately smaller than a key store: no enumeration, no read, no
export. A tool that needs the plaintext does not get to be a Spore.

## Contract

`wit/soil-creds.wit` (`soil:creds@1.0.0`):

| Call | Result |
|---|---|
| `resolve(name)` | `handle { id, purpose, expires-at-ms }`, or `denied` / `expired` / `unavailable` |
| `sign(handle, algorithm, message)` | signature bytes |
| `verify(handle, algorithm, message, signature)` | `ok(true)` / `ok(false)` |

The handle is a host id, never a secret, path, or key name a guest can forge.
A *mismatched* signature is `ok(false)`; the errors describe the credential or
the request, not the answer. A guest that treats `ok(false)` as an error will
log invalid signatures as outages.

Algorithms today: `hmac-sha256`, `ed25519`.

## Host side

[`CredentialProvider`](../crates/soil-wasi-host/src/creds.rs) is the trait a host
implements: `resolve`, `sign`, `verify`. Two implementations:

| Impl | Use | Secrets live |
|---|---|---|
| `StaticCredentials` | local operator runs and tests | in the host process, from `insert_hmac` |
| `GeodeCredentials` | production wiring | in **Geode**, behind the trait |

**Status:** `StaticCredentials` is built. `GeodeCredentials` is designed and not
yet implemented; until it lands, an ingress reads its secret from its own
configuration and inserts it at startup.

### How a Geode-backed resolve works

1. The guest calls `resolve("confluence.webhook/smoke")`.
2. The host turns the name into a `GeodeRef` and calls
   `Store::lease(&geode_ref, &Purpose, Duration)`. Geode checks that the entry
   lists that purpose, that the lease is within `MIN_LEASE..MAX_LEASE`
   (1 s .. 1 h), and records the decision in its `AuditLog`.
3. The returned `SecretBytes` is used for exactly one `sign`/`verify` and then
   dropped; it zeroizes. It is never cached past the lease.
4. `denied` and `expired` map to the WIT errors of the same name; an unreachable
   backend is `unavailable`, never a silent unsigned fallback.

Two facts make this safe to put in front of a webhook: the read is
purpose-gated (a credential minted for verification cannot be used to sign), and
it is time-bounded (a leaked handle is useless after the lease).

## Where secrets must not live

- **Records or appdata.** `soil:records` is synced and readable by any Spore
  holding the grant. Store the credential *name* there; the name is not a
  secret.
- **Logs.** Never log a resolved secret, a signature key, or a full header set.
  The ingress log line carries `verifier=` and a delivery id, nothing else.
- **The filesystem beside a registry.** `~/confluence/config/webhooks/*.json`
  plus a `0600` secret beside each entry is a stand-in for this contract, kept
  only while `GeodeCredentials` does not exist. When it lands, that directory
  becomes names-only and the secrets move behind the trait.
- **Unit files and environment.** `CONFLUENCE_WEBHOOK_SECRET` exists for local
  runs. A production unit should name a Geode entry instead.

## What a Spore can and cannot learn

A Spore can learn: whether a name is granted, a handle id, the purpose string,
an absolute expiry, and a boolean answer to a verification.

A Spore cannot learn: the secret, its length, how it was generated, whether two
handles share material, or what other credentials exist on the host.
