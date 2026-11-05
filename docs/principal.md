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
