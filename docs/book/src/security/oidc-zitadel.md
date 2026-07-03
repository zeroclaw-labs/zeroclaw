# OIDC with Zitadel

Wire a Zitadel instance to ZeroClaw as an `[oidc.<alias>]` trust
relationship. Read [Authentication](./authentication.md) first for the model;
this page is only the Zitadel-side setup and the values it produces.

## What you are producing

| ZeroClaw field | Zitadel source |
|---|---|
| `issuer` | `https://<zitadel-host>` (your instance's custom domain) |
| `audience` | The application's Client ID |
| `claim_path` | `urn:zitadel:iam:org:project:roles` (see below) |
| `role_map` | Zitadel project role keys → your permission profile aliases |

## 1. Create the project and application

In the Zitadel console:

1. **Projects → Create** a project for ZeroClaw.
2. Inside the project, **New application**:
   - For interactive users via the **device authorization grant**: type
     **Native** (or Web with the device grant enabled), authentication
     method **None** (public client). Enable **Device Code** in the grant
     types.
   - For headless **client credentials** or `introspection` validation:
     type **API**, authentication method **Basic**; copy the generated
     client secret.
3. The generated **Client ID** becomes `audience`.

## 2. Put roles in the token

Zitadel expresses authorization as project roles:

1. In the project, **Roles → New role** per ZeroClaw permission tier (the
   **key** is what appears in the token, e.g. `zeroclaw-admin`).
2. Grant roles to users under **Authorizations**.
3. On the project, enable **Assert Roles on Authentication** so the roles
   claim is present in tokens.

The roles claim is `urn:zitadel:iam:org:project:roles` and its value is an
object keyed by role name. The claim name contains no dots, so set
`claim_path` to the claim name verbatim; ZeroClaw extracts the object's keys
as the role values, so each granted role key is matched against `role_map`
directly.

## 3. Map roles to profiles

Each `role_map` entry maps one role key to one
`[permission_profiles.<alias>]` name. A token whose roles match no entry is
denied.

## 4. Choose validation

- `jwks` (default): fine for public clients; Zitadel publishes keys via the
  discovery document.
- `introspection`: requires the API application's Basic credentials as
  `client_secret`; gives immediate revocation.

## 5. MFA

Zitadel stamps `amr` with the factors used (e.g. `otp`, `user`). Set
`require_mfa` only after confirming a real token carries `mfa`, `otp`, or
`hwk` when your login policy enforces a second factor.

## Troubleshooting

- **Issuer mismatch**: use the exact instance domain from the token's `iss`
  claim; Zitadel custom domains change it.
- **Roles claim absent**: "Assert Roles on Authentication" is off, or the
  user has no authorization in the project.
- **Device flow rejected**: the application's grant types do not include
  device code.
