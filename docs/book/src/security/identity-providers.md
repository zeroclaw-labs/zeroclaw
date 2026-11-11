# Identity providers

ZeroClaw signs people in through OpenID Connect, and it is strict about the access token a provider issues. Most providers need settings changed, and some don't work yet. This page lists what a provider needs, the providers tested with v0.9.0 pre-release builds, the ones known not to work yet, and the current limitations. Only the providers under [Tested providers](#tested-providers) are known to work. To configure a provider, see [OIDC](./authentication.md#oidc) and [Dashboard sign-in](./authentication.md#dashboard-sign-in).

The daemon verifies every access token with the same rules, whether it arrives through dashboard sign-in or from a native client (`zerocode`, or a token from `zeroclaw oidc login`), so a provider whose access token is refused is refused everywhere. `zeroclaw oidc login` and `zeroclaw oidc token` only fetch a token and print it. A sign-in that succeeds there doesn't show that the daemon accepts the token.

## What a provider needs

- **Discovery.** `<issuer>/.well-known/openid-configuration` must name exactly the configured `issuer`, trailing slash included.
- **PKCE with S256, for browser sign-in.** Dashboard sign-in, `zeroclaw oidc login --browser` and the gateway's browser enrollment need the discovery document to list `S256` in `code_challenge_methods_supported`. They refuse an issuer that leaves the field out, because a missing field means no PKCE, and `plain` is never accepted. The device grant and `client_credentials` don't read this field.
- **The device grant, for `zerocode` and `zeroclaw oidc login`.** `zerocode`, and `zeroclaw oidc login` without `--browser`, sign in with the Device Authorization Grant (RFC 8628), so the provider must offer it and list `device_authorization_endpoint` in discovery. Dashboard sign-in doesn't use it.
- **An access token ZeroClaw can verify,** through one of the two `validation` modes:
  - `jwks`: an RFC 9068 JWT access token, with the header `typ` `at+jwt` (or `application/at+jwt`), signed with RS256 or ES256. It must carry `iss`, `aud`, `exp`, `iat` and `jti`, and name its client in `client_id` (for a person's token, `azp` is accepted instead). Every key in the issuer's key set needs its own `kid`. A JWT typed `JWT` or with no `typ`, which is what many providers issue by default, is refused, because it can't be told apart from an ID token.
  - `introspection`: RFC 7662 token introspection whose answer carries `active: true`, `token_type: Bearer`, the configured `audience` and a `client_id`. The discovery document must list `introspection_endpoint`. This mode needs the entry's `client_secret`.
- **Nothing that marks it as another kind of token.** In either mode, the token (or the introspection answer) must not carry a `nonce` claim. A `typ` or `token_use` claim, if present, must name an access token: `typ` `Bearer` or `at+jwt`, and `token_use` exactly `access`.
- **The audience.** The access token's `aud` must contain the configured `audience`.
- **Roles or groups without an extra scope.** ZeroClaw requests only the `openid` scope, so the values at `claim_path` must be in the access token (or the introspection answer) by default. Otherwise, map people one by one, for example with `claim_path = "sub"`.
- **Client authentication by request.** The authorization-code exchange (dashboard sign-in, `zeroclaw oidc login --browser` and the gateway's browser enrollment) sends the `client_secret` in the form body (`client_secret_post`). Introspection, the device grant and `client_credentials` send it with HTTP Basic (`client_secret_basic`). A provider that accepts only the method registered for a client may need a separate `[oidc.<alias>]` entry and client for the device grant or `client_credentials`. Dashboard sign-in with `introspection` can't be split that way: one client makes both the exchange and the introspection call, so it has to accept both methods.
- **A working-session token lifetime,** such as one to eight hours. The dashboard doesn't use refresh tokens, so a session ends when its access token expires.

## Tested providers

Each provider below signed in an administrator and a member account through dashboard sign-in, with the profiles their claims map to. On the bearer path, the same provider's ID token, refresh token, and an access token issued to another of its clients were each refused. The tests ran against v0.9.0 pre-release builds in October and November 2026. Keycloak was tested with the gateway on plain HTTP and on its own TLS listener, and with `zerocode` and `zeroclaw oidc login`. Zitadel and Authelia were tested with dashboard sign-in only.

| Provider | Version tested | `validation` |
|---|---|---|
| [Keycloak](#keycloak) | 26.8.0 | `jwks` or `introspection` |
| [Zitadel](#zitadel), self-hosted | v4.20.1 | `introspection` |
| [Authelia](#authelia) | 4.39.28 | `jwks` |

### Keycloak

- Turn on the client's RFC 9068 access-token header (`access.token.header.type.rfc9068 = true`), require PKCE `S256`, and set an access-token lifespan of one to eight hours. Keycloak's default is five minutes.
- Add an audience mapper that puts the configured `audience` in the access token.
- Add a group membership mapper with the token claim name `groups`, without full paths, for `claim_path = "groups"`.
- The tested client also had a hard-coded `client_id` claim mapper. Without one, see [Keycloak access-token client attribution](./authentication.md#keycloak-access-token-client-attribution).
- For `introspection`, also add an audience mapper with the client's own client ID. Keycloak 26.6.2 and later answer introspection as inactive for a token that doesn't name the asking client.

### Zitadel

- Create a Web application whose authentication method is Code (HTTP Basic) or Post. Both were tested. Don't pick the PKCE method: it creates a client without a secret, and `introspection` needs one. ZeroClaw sends a PKCE challenge with either.
- Give each person a role in the project. The tested project also had role assertion turned on.
- Set `audience` to the project ID and `claim_path = "urn:zitadel:iam:org:project:roles"`.
- `jwks` doesn't work: Zitadel's access tokens are opaque, or JWTs typed `JWT`.

### Authelia

- Register the client with `token_endpoint_auth_method: client_secret_post`, `access_token_signed_response_alg: RS256` and PKCE `S256`. The default `client_secret_basic` fails the code exchange.
- Set the client's `audience` to the configured `audience`, with `requested_audience_mode: implicit`. Authelia's default, `explicit`, adds an audience only when the client asks for one, and ZeroClaw doesn't.
- Map people one by one (`claim_path = "sub"`). Authelia's group claim needs a `groups` scope, which ZeroClaw doesn't request.
- `introspection` doesn't work: Authelia's introspection answer has no `token_type`.
- Authelia serves its issuer over HTTPS only, so its certificate must chain to a certificate authority the daemon trusts (see [Limitations](#limitations-in-v090)).

## Known not to work yet

These were tested and have no configuration that works in v0.9.0:

- **Dex** (v2.46.1). Its JWT access tokens have no `typ` in their header and carry the sign-in nonce, and its introspection marks tokens `token_use: access_token` while answering for its ID tokens the same way. Accepting that would let an ID token be used as an access token, so it stays refused.
- **Authentik** (2026.8.5). Its access token is typed `JWT` and carries the sign-in nonce, and its introspection answer has no `token_type`.
- **Logto** (1.45.0, tested with `zeroclaw oidc login` and not with the dashboard). Its access tokens for people are opaque unless the token request names an RFC 8707 `resource`, which ZeroClaw doesn't send yet. So people can't sign in. A Logto service client using `client_credentials` works only with a default API resource set and Logto's signing key rotated to RSA: its default key signs with ES384, which ZeroClaw doesn't verify.

Not tested. Going by their documentation, published discovery documents and source, these aren't expected to work yet:

- **Amazon Cognito.** Its discovery document doesn't list `S256`, its access tokens have no `typ` in their header, and it has no introspection endpoint.
- **Ory Hydra** (v26.2.0). Its JWT access tokens are typed `JWT`, and its introspection is served only on its admin port and isn't in discovery.

Any other provider, hosted or self-hosted, hasn't been tested. Check it against [What a provider needs](#what-a-provider-needs). The usual blockers are access tokens that aren't typed `at+jwt`, opaque tokens without introspection, and discovery that doesn't advertise `S256`.

## Limitations in v0.9.0

- **No RFC 8707 `resource` parameter.** Providers that issue a verifiable access token only for a named resource, such as Logto, can't sign people in yet.
- **No configurable scopes.** Every request asks for `openid` only, so claims that a provider releases only for another scope (often `groups`) never arrive.
- **No private certificate authority per provider.** The issuer's TLS certificate must chain to a certificate authority the daemon already trusts. There's no setting to add one for a single provider.
- **The CLI's gateway admin commands don't work when the gateway serves TLS itself** ([#12091](https://github.com/zeroclaw-labs/zeroclaw/issues/12091)).
  - The affected commands are `zeroclaw gateway get-paircode` in every form, `zeroclaw sop pending`, `logs`, `approve` and `deny`, and `zeroclaw gateway restart`.
  - They always connect over plain `http://`, so with `[gateway.tls]` enabled they fail with "Failed to connect to gateway".
  - `zeroclaw gateway restart` reads that failure as "no gateway running" and tries to start a second one on the same address.
- **A provider outage when a session is verified again signs dashboard users out** ([#12096](https://github.com/zeroclaw-labs/zeroclaw/issues/12096)).
  - The gateway verifies a signed-in session's token again:
    - at each introspection revalidation deadline (`revalidation_secs`);
    - after a configuration reload, in either `validation` mode;
    - when its grants check fails because the policy changed (a change to `[oidc]`, `[users]`, `[permission_profiles]` or `security.trust_daemon_uid`).
  - If the provider can't be reached then, or answers with an error, the session ends. It fails closed: nothing is admitted that shouldn't be.
  - The dashboard reports the session as expired. The gateway logs the real cause as `dashboard session ended: verifying its token again failed`.
  - Users sign in again once the provider is back.
  - Without a reload or a policy change, a `jwks` session isn't verified again. It keeps its verified identity until its token or the session expires, however long the provider is down.
  - The five-minute key cache matters only for a token that is presented fresh, such as a new `zerocode` connection. That token is verified from cached keys for up to five minutes after the last key fetch, and refused after that until the provider answers. A reload or a policy change starts with no cached keys.
- **A scoped account's dashboard chat lasts only as long as its connection.** Reloading the page, leaving the chat, or a dropped connection ends the conversation, and a new chat starts a new one. The transcript is kept, but the dashboard can't reopen it. See [Dashboard sign-in](./authentication.md#dashboard-sign-in).
