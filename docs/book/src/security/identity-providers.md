# Identity providers

ZeroClaw signs people in through any OpenID Connect provider that meets the requirements below. This page lists the providers that have been tested against v0.9.0, the ones known not to work yet, and the current limitations. To configure a provider, see [OIDC](./authentication.md#oidc) and [Dashboard sign-in](./authentication.md#dashboard-sign-in).

Dashboard sign-in and the native clients (`zerocode` and `zeroclaw oidc login`) verify the provider's access token with the same rules, so a provider whose access token is refused is refused everywhere.

## What a provider needs

- **Discovery.** `<issuer>/.well-known/openid-configuration` must name exactly the configured `issuer`, trailing slash included.
- **PKCE with S256.** The discovery document must list `S256` in `code_challenge_methods_supported`. An issuer that leaves the field out is refused, because a missing field means no PKCE, and `plain` is never accepted.
- **An access token ZeroClaw can verify,** through one of the two `validation` modes:
  - `jwks`: an RFC 9068 JWT access token, with the header `typ` `at+jwt` (or `application/at+jwt`), signed with RS256 or ES256. Every key in the issuer's key set needs its own `kid`. A JWT typed `JWT`, which is what many providers issue by default, is refused, because it can't be told apart from an ID token.
  - `introspection`: RFC 7662 token introspection whose answer carries `active: true`, `token_type: Bearer`, the configured `audience` and a `client_id`. This mode needs the entry's `client_secret`.
- **The audience.** The access token's `aud` must contain the configured `audience`.
- **Roles or groups without an extra scope.** ZeroClaw requests only the `openid` scope, so the values at `claim_path` must be in the access token (or the introspection answer) by default. Otherwise, map people one by one, for example with `claim_path = "sub"`.
- **Client authentication by request.** The authorization-code exchange (dashboard sign-in and `zeroclaw oidc login --browser`) sends the `client_secret` in the form body (`client_secret_post`). Introspection, the device grant and `client_credentials` send it with HTTP Basic (`client_secret_basic`). A provider that accepts only the method registered for the client may need a separate client for each kind of request.
- **A working-session token lifetime,** such as one to eight hours. The dashboard doesn't use refresh tokens, so a session ends when its access token expires.

## Tested providers

Each provider below signed in an administrator and a member account through dashboard sign-in, with the profiles their claims map to. On the bearer path, the same provider's ID token, refresh token, and an access token issued to another of its clients were each refused. The tests ran against v0.9.0 pre-release builds in October and November 2026. Keycloak was tested with the gateway on plain HTTP and on its own TLS listener.

| Provider | Version tested | `validation` |
|---|---|---|
| [Keycloak](#keycloak) | 26.8.0 | `jwks` or `introspection` |
| [Zitadel](#zitadel), self-hosted | v4.20.1 | `introspection` |
| [Authelia](#authelia) | 4.39.28 | `jwks` |

### Keycloak

- Turn on the client's RFC 9068 access-token header (`access.token.header.type.rfc9068 = true`), require PKCE `S256`, and set an access-token lifespan of one to eight hours. Keycloak's default is five minutes.
- Add an audience mapper that puts the configured `audience` in the access token.
- Add a group membership mapper named `groups`, without full paths, for `claim_path = "groups"`.
- The tested client also had a hard-coded `client_id` claim mapper. Without one, see [Keycloak access-token client attribution](./authentication.md#keycloak-access-token-client-attribution).
- For `introspection`, also add an audience mapper with the client's own client ID. Keycloak 26.6.2 and later answer introspection as inactive for a token that doesn't name the asking client.

### Zitadel

- Create a Web application with Authorization Code and PKCE. Client authentication Basic or Post both work.
- Turn on role assertion for the project.
- Set `audience` to the project ID and `claim_path = "urn:zitadel:iam:org:project:roles"`.
- `jwks` doesn't work: Zitadel's access tokens are opaque, or JWTs typed `JWT`.

### Authelia

- Register the client with `token_endpoint_auth_method: client_secret_post`, `access_token_signed_response_alg: RS256` and PKCE `S256`. The default `client_secret_basic` fails the code exchange.
- Grant the client the configured `audience`.
- Map people one by one (`claim_path = "sub"`). Authelia's group claim needs a `groups` scope, which ZeroClaw doesn't request.
- `introspection` doesn't work: Authelia's introspection answer has no `token_type`.
- Authelia serves its issuer over HTTPS only, so its certificate must chain to a certificate authority the daemon trusts (see [Limitations](#limitations-in-v090)).

## Known not to work yet

These were tested and have no configuration that works in v0.9.0:

- **Dex** (v2.46.1). Its JWT access tokens are typed `JWT`, and its introspection marks tokens `token_use: access_token` while answering for its ID tokens the same way. Accepting that would let an ID token sign in, so it stays refused.
- **Authentik** (2026.8.5). Its access token is typed `JWT` and carries the sign-in nonce, and its introspection answer has no `token_type`.
- **Logto.** Its access tokens for people are opaque unless the token request names an RFC 8707 `resource`, which ZeroClaw doesn't send yet. So people can't sign in. Logto service clients using `client_credentials` work.

Not tested, and not expected to work yet:

- **Amazon Cognito.** Its discovery document doesn't list `S256`, and it has no introspection endpoint.
- **Ory Hydra.** Its JWT access tokens are typed `JWT`, and its introspection is served only on its admin port and isn't in discovery.

Any other provider, hosted or self-hosted, hasn't been tested. Check it against [What a provider needs](#what-a-provider-needs). The usual blockers are access tokens typed `JWT` or opaque without introspection, and discovery that doesn't advertise `S256`.

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
    - when its grants check fails because the policy changed.
  - If the provider can't be reached then, or answers with an error, the session ends. It fails closed: nothing is admitted that shouldn't be.
  - The dashboard reports the session as expired. The gateway logs the real cause as `dashboard session ended: verifying its token again failed`.
  - Users sign in again once the provider is back.
  - Without a reload, `jwks` mode keeps verifying from its cached keys for up to five minutes.
- **A scoped account's dashboard chat lasts only as long as its connection.** Reloading the page, leaving the chat, or a dropped connection ends the conversation, and a new chat starts a new one. The transcript is kept, but the dashboard can't reopen it. See [Dashboard sign-in](./authentication.md#dashboard-sign-in).
