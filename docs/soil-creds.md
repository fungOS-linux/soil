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
| `sign-pgp(handle, message)` | detached armored signature |
| `verify-pgp(handle, message, signature)` | `ok(true)` / `ok(false)` |

The handle is a host id, never a secret, path, or key name a guest can forge.
A *mismatched* signature is `ok(false)`; the errors describe the credential or
the request, not the answer. A guest that treats `ok(false)` as an error will
log invalid signatures as outages.

Algorithms: `hmac-sha256`, `ed25519`. PGP is a separate pair of calls,
`sign-pgp` and `verify-pgp`, so the existing `algorithm` enum stays
ABI-compatible with guests built before PGP landed. The host still owns the key
and returns only the signature.

## Host side

[`CredentialProvider`](../crates/soil-wasi-host/src/creds.rs) is the trait a host
implements: `resolve`, `sign`, `verify`, `sign_pgp`, `verify_pgp`. Two
implementations:

| Impl | Use | Secrets live |
|---|---|---|
| `StaticCredentials` | local operator runs and tests | in the host process, from `insert_hmac` |
| `GeodeCredentials` | HMAC/Ed25519 leases | in **Geode**, leased per handle |
| `GeodePgpCredentials` | PGP signing | in **Geode**, through `geode-pgp` |

**Status:** all three are built. `GeodeCredentials` leases a secret from a
running `geoded` for a handle. `GeodePgpCredentials` invokes `geode-pgp`, which
leases the key for one signature; the host and guest never see the key bytes.

### How a Geode-backed resolve works

1. The guest calls `resolve("confluence.webhook/smoke")`.
2. The host maps the granted name to a non-secret `geode://` reference and
   purpose. `geode-pgp` asks `geoded` to lease the key with that purpose; Geode
   checks the entry allowlist and the lease bounds, and records the decision in
   its `AuditLog`.
3. The key is used for exactly one signature or verification and then dropped.
   It is never cached past the lease, and no key bytes enter the host or guest.
4. `denied` and `expired` map to the WIT errors of the same name; an unreachable
   backend is `unavailable`, never a silent unsigned fallback.

Two facts make this safe to put in front of a webhook: the read is
purpose-gated (a credential minted for verification cannot be used to sign), and
it is time-bounded (a leaked handle is useless after the lease).

## PGP and the principal plane

The same host-held path signs Spore packages and store delegations. The guest
never names a key path or sees key material.

- `soil:principal` projects the actor and, separately, the non-secret signing-key
  metadata: algorithm, fingerprint and generation. It does not put key material
  in the `identity` record.
- `soil:creds` resolves a host-configured name to a Geode entry. `GeodePgpCredentials`
  holds an explicit name -> `(reference, purpose, fingerprint)` mapping; the host
  builds that mapping from `soil:principal.signing-key()` and its policy. The guest
  never supplies a key path or another principal's reference.
- `geode-pgp` (alongside the existing passkey/OTP/OAuth connectors) leases the
  armored private key for one detached signature and drops it. Only the signature
  crosses back. Geode remains the owner; the connector owns the OpenPGP format.
- The same signing path covers `spore.package.sign`, `store.delegation.sign` and
  the existing `webhook.sign` purposes. The purpose is the grant.

Verification uses the public key or the fingerprint in a delegation; a consumer
does not need Geode. That is what makes a federated store work: the operator key
signs delegations, publisher keys sign packages, and any store or mirror can pin
the resulting CIDs.

## Where secrets must not live

- **Records or appdata.** `soil:records` is synced and readable by any Spore
  holding the grant. Store the credential *name* there; the name is not a
  secret.
- **Logs.** Never log a resolved secret, a signature key, or a full header set.
  The ingress log line carries `verifier=` and a delivery id, nothing else.
- **The filesystem beside a registry.** `~/confluence/config/webhooks/*.json`
  plus a `0600` secret beside each entry is a stand-in for this contract. The
  Geode-backed provider now exists; that directory should become names-only and
  the secrets should move behind the trait.
- **Unit files and environment.** `CONFLUENCE_WEBHOOK_SECRET` exists for local
  runs. A production unit should name a Geode entry instead.

## What a Spore can and cannot learn

A Spore can learn: whether a name is granted, a handle id, the purpose string,
an absolute expiry, a boolean answer to a verification, and its own principal's
public signing fingerprint.

A Spore cannot learn: the secret, its length, how it was generated, whether two
handles share material, another principal's key material, or what other
credentials exist on the host.
