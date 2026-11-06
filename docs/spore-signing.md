# Spore publisher signing

Spore packages can carry a delegated publisher signature. The package leaf is
signed by a publisher key; the host verifies that key with a `PublisherTrust`
provider.

```text
Mycelium signed publisher grant
  -> delegated publisher key
    -> statement: spore id + version + package digest + principal + expiry
      -> package signature
```

`soil-spore` owns only the leaf verification and package binding. It does not
own the authorization table, signing keys, delegation or revocation.

## Statement

The statement binds:

```text
spore_id
version
package_digest      // spore.lock digest
principal           // Mycelium/OIDC actor
grant_id
serial
publisher_key       // hex ed25519 delegated key
not_before
not_after
```

The signature covers canonical JSON of that statement with the delegated
publisher key. The host then checks the statement against its trust provider.

## Host trust

`PublisherTrust::authorize(statement, now)` is the Mycelium integration point.
It must verify:

- the signed publisher grant
- delegation from the Mycelium principal to the publisher key
- exact Spore id, role and scope
- grant serial / generation
- expiry and revocation

`Ok` means the exact statement is still authorized. `Err` fails closed.

## System Spores

A manifest may declare a system role:

```toml
[system]
role = "consent"
```

That is not a grant. The host's `SystemAllowlist` must also match the exact:

```text
Spore id
role
publisher principal
package digest
```

An unsigned or self-declared system Spore is not trusted.

## Current scope

Implemented in `soil-spore`:

- ed25519 leaf signatures
- statement/package binding
- expiry check
- `PublisherTrust` trait
- `SystemAllowlist`

Not yet implemented:

- concrete Mycelium `PublisherTrust` adapter
- signing CLI
- Spore Store installation flow using the verified publisher
