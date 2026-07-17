# OIDC with Keycloak

Wire a Keycloak realm to ZeroClaw as an `[oidc.<alias>]` trust relationship.
Read [Authentication](./authentication.md) first for the model; this page is
only the Keycloak-side clicks and the values they produce.

## What you are producing

Four values for the ZeroClaw side, set through any surface listed on the
[Authentication](./authentication.md) page:

| ZeroClaw field | Keycloak source |
|---|---|
| `issuer` | `https://<keycloak-host>/realms/<realm>` |
| `audience` | The client ID you create below |
| `claim_path` | `realm_access.roles` for realm roles, `groups` if you map a groups claim |
| `role_map` | Realm role names → your permission profile aliases |

## 1. Create the client

In the Keycloak admin console, under your realm:

1. **Clients → Create client.** Client type **OpenID Connect**, pick a client
   ID (this becomes `audience`).
2. On the capability screen the choice depends on the flow:
   - For interactive users enrolling via the **device authorization grant**,
     enable **OAuth 2.0 Device Authorization Grant** and leave the client
     public (no client authentication).
   - For headless service accounts via **client credentials**, enable
     **Client authentication** and **Service accounts roles**. Copy the
     generated secret from the **Credentials** tab; it becomes the entry's
     `client_secret`.
3. Token introspection (`validation = introspection`) also requires the
   confidential client with a secret.

## 2. Put roles in the token

Keycloak places realm roles at `realm_access.roles` in the access token by
default, which is exactly what `claim_path` expects.

1. **Realm roles → Create role** for each ZeroClaw permission tier, e.g.
   `zeroclaw-admin`, `zeroclaw-operator`.
2. Assign the roles to users (**Users → Role mapping**) or groups.
3. Verify the audience: by default Keycloak may not stamp your client ID into
   `aud`. If tokens fail audience validation, add a mapper: **Clients → your
   client → Client scopes → dedicated scope → Add mapper → Audience**, and
   include your client ID.

Using groups instead of roles: add a **Group Membership** mapper to the
client scope with token claim name `groups` (full path off), and set
`claim_path` to `groups`.

## 3. Map roles to profiles

Each `role_map` entry maps one claim value to one
`[permission_profiles.<alias>]` name, e.g. `zeroclaw-admin` → an admin
profile, `zeroclaw-operator` → a scoped profile. A token whose roles match no
entry is denied.

## 4. Choose validation

- `jwks` (default): no secret needed for a public client. Keycloak publishes
  keys at the discovery document; rotation is handled automatically.
- `introspection`: requires the confidential client and secret; gives
  immediate revocation when a Keycloak session is logged out.

## 5. MFA

If your realm enforces OTP/WebAuthn, Keycloak stamps `amr` accordingly and
you can set `require_mfa` on the ZeroClaw entry. Verify your realm's browser
flow actually records the factor: an `amr` without `mfa`, `otp`, or `hwk` is
rejected when `require_mfa` is on.

## Troubleshooting

- **Token denied with issuer mismatch**: `issuer` must match the token's
  `iss` claim byte-for-byte, including the `/realms/<realm>` suffix and no
  trailing slash.
- **Audience failures**: inspect an actual access token (base64-decode the
  payload) and confirm `aud` contains your client ID; add the audience
  mapper if not.
- **Roles missing**: confirm the roles are realm roles (not client roles) or
  adjust `claim_path` to `resource_access.<client>.roles` for client roles.
