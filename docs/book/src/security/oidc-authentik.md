# OIDC with Authentik

Wire an Authentik instance to ZeroClaw as an `[oidc.<alias>]` trust
relationship. Read [Authentication](./authentication.md) first for the model;
this page is only the Authentik-side setup and the values it produces.

## What you are producing

| ZeroClaw field | Authentik source |
|---|---|
| `issuer` | `https://<authentik-host>/application/o/<application-slug>/` |
| `audience` | The provider's Client ID |
| `claim_path` | `groups` (Authentik's default scope mapping) |
| `role_map` | Authentik group names → your permission profile aliases |

Note the issuer ends with a trailing slash; Authentik includes it in `iss`,
so include it in `issuer` too.

## 1. Create the provider and application

In the Authentik admin interface:

1. **Applications → Providers → Create**, type **OAuth2/OpenID Provider**.
   - For interactive users via the **device authorization grant**: client
     type **Public**. Also assign a flow under **Flow settings → Device code
     flow** (Authentik requires an explicit device-authorization flow;
     without one the device endpoint is absent and enrollment fails with a
     missing-endpoint error).
   - For headless **client credentials** or `introspection` validation:
     client type **Confidential**; copy the generated client secret.
2. **Applications → Applications → Create**, bind it to the provider. The
   application **slug** determines the issuer URL.
3. The provider's **Client ID** becomes `audience`.

## 2. Put groups in the token

Authentik ships a default scope mapping that emits the user's group names as
the `groups` claim, so `claim_path` is simply `groups`. Confirm the provider
includes the default `openid`, `profile`, and the groups-bearing scope
mapping under **Advanced protocol settings → Scopes**.

Create a group per ZeroClaw permission tier (e.g. `zeroclaw-admins`) under
**Directory → Groups** and add users.

## 3. Map groups to profiles

Each `role_map` entry maps one group name to one
`[permission_profiles.<alias>]` name. A token whose groups match no entry is
denied.

## 4. Choose validation

- `jwks` (default): works with a public client, no secret. Ensure the
  provider's signing key is set (Authentik selects a certificate under
  **Advanced protocol settings → Signing Key**).
- `introspection`: requires the confidential client and its secret; revokes
  immediately when the session ends in Authentik.

## 5. MFA

Authentik records authentication methods in `amr` when the flow includes an
authenticator stage (TOTP, WebAuthn). Set `require_mfa` on the ZeroClaw entry
only after confirming a real token carries `mfa`, `otp`, or `hwk` in `amr`.

## Troubleshooting

- **Issuer mismatch**: Authentik's issuer is per-application
  (`/application/o/<slug>/`), not the bare host. Decode a token and copy
  `iss` exactly.
- **Device flow fails to start**: the provider has no device code flow
  assigned. Assign one under the provider's flow settings.
- **Groups claim empty**: the groups scope mapping is not selected on the
  provider, or the client did not request the scope.
