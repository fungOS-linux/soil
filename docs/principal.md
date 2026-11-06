# Principal

`soil:principal@0.1.0` is the host-attested "who" for a Spore invocation.

Mycelium identity / OIDC is the source of truth. The host verifies the token or
device identity and exposes only a narrow projection. A Spore never sees a JWT,
refresh token, raw claims, client secret or device key.

```wit
current() -> {
  actor,
  kind,          // human | agent | service
  display-name,
  device,
  teams,
  roles,
  generation,
}
```

```text
actor        OIDC iss#sub or Mycelium principal id
kind         human, agent or service
display-name presentation only, never email
device       host/device identity
teams        memberships used to select books
roles        host-mapped roles, not raw OIDC groups
generation   changes on re-auth/team/role/device change
```

## What it is not

```text
not a grant
not a permission
not a token
not a claim bag
not caller-selected
```

Principal answers **who**. Grants answer **what**. Books answer **where**.

## OIDC mapping

The host verifies issuer, audience, signature and expiry, then maps:

```text
iss + sub          -> actor: oidc:<iss>#<sub>
name               -> display-name
preferred_username -> display-name fallback
teams[]            -> teams
roles[]            -> roles
host config        -> device
```

Email is deliberately not exposed. The host may map additional claims, but only
through an explicit local policy.

## Mycelium mapping

A verified Mycelium grant already carries a principal, roles, scopes and a
device/transport identity. The host maps those into the same structure. A
device or peer identity authenticates a machine; it does not by itself grant
human permissions.


## Mycelium trust document

`soil-principal-host` has an optional `mycelium` feature that reads the same
retained `AccessRecord` trust document used by the records peer host:

```rust
let provider = MyceliumTrustProvider::new(
    trust_file,
    issuer_public_keys,
    PrincipalKind::Human,
    Some(device_id),
);
```

It verifies the grant signature and issuer set, rejects revoked or expired
grants, and maps:

```text
grant.principal -> actor
grant.roles     -> roles
team:<id> scopes -> teams
grant.serial    -> generation
host device     -> device
```

Team extraction is a host policy. The default scope prefix is `team:` and can
be changed with `with_team_scope_prefix`.

```sh
cargo test -p soil-principal-host --features mycelium
```


## JWT verification through Mycelium

Loose coupling to Mycelium uses its stable CLI contract:

```sh
mycelium access oidc verify \
  --issuer https://identity.example \
  --audience soil \
  --token-env SOIL_OIDC_TOKEN \
  --json
```

`MyceliumCliVerifier` invokes that command without linking Mycelium code or
touching a daemon socket. The JWT is passed only through the child process
environment, never argv, WIT, logs, or persistence.

The parsed contract is:

```text
principal
issuer
subject
email       // ignored by the Soil projection
groups      // mapped to roles
expires_at
```

Then:

```rust
let verified = MyceliumCliVerifier::new("mycelium")
    .verify(token, issuer, audience)?;
let principal = Principal::from_verified_oidc(
    verified,
    PrincipalKind::Human,
    Some(device_id),
    generation,
)?;
```

The verifier rejects:

```text
non-zero exit
oversized output
malformed JSON
expired token
principal that does not match oidc:<issuer>#<subject>
```

This keeps Mycelium as the OIDC implementation and Soil as the host projection.

## Host integration

```text
Mycelium / OIDC
  -> verification
  -> Principal
  -> soil:principal host
  -> Spore
```

The host attaches the same principal to:

```text
notification recipient
Marbles claim/assignee
action approval and audit
team book selection
mention notification
```

## Mention targets

A CR contact is not automatically a principal. A target is notifiable only when
it is explicitly linked to a principal:

```text
record:personal/avery/crm#c-ada   contact
principal:oidc:issuer#subject-42  principal
```

Mentioning a contact does not notify anyone. Mentioning a principal can.
