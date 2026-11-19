# Browser credential consumer design

This is a design for a follow-up to RFC #12422 and the extended credential catalog in #12517.

## Operator grants

In **Settings → Secrets → Permissions**, give username/password entries a browser-login grant and TOTP entries a browser-verification grant. Each grant defaults to denied and is scoped to an exact HTTPS origin, including its effective port. An operator can enable either grant or both independently. The HTTP Request grant stays independent. A destination change clears the affected browser grant. An unsupported backend shows that browser credential use is unavailable.

## Runtime boundary

Use an isolated managed browser context and trusted, purpose-built login operations. The caller supplies an alias. The executor resolves it just in time from the existing catalog and configured vault. Treat the seed as a secret and keep generated codes inside the executor. A password operation and a TOTP operation check their own grants, including after redirects and on multi-step login flows. A challenge displayed by a page carries no authority. Bind both the active top-level origin and the receiving document and submission destination to the approved HTTPS origin. Reject unsupported frames, insecure transport, certificate errors and ambiguous destinations. Retain existing browser policies and operator approvals. A delegated or resumed task must pass the same checks against current policy.

Keep credential-bearing pages and browser session material out of generic snapshots, screenshots, downloads, script evaluation, and tool results during the credential operation. Verify that secrets never enter process arguments, diagnostics, telemetry, model context, or persisted browser profiles. Start with a backend that can enforce these properties and fail closed elsewhere. Return only a bounded status. A successful fill is distinct from successful authentication. Support a TOTP-only grant for a session already at a verification step. Keep browser sessions isolated by task and close them on termination.

## Acceptance tests

Use disposable HTTPS fixtures. Cover password-only, TOTP-only, both grants and neither. Exercise same-page and staged challenges, revoked grants, changed destinations, cross-origin redirects and frames, backend refusal, cancellation, clock rollover, local and external vault resolution, and absence of credential material in every agent-visible output. Keep existing HTTP and SSH grants independent.

## Integration status

The draft implements separate catalog grants and a Rust predicate over the top-level URL, credential document, and submission target. The predicate is evaluated against their HTTPS origins. It does not establish that the URLs were collected atomically or that the browser prevents subsequent script-driven transmission. Those guarantees belong to the managed browser executor. The ordinary fill action remains separate from the credential flow.

The initial acceptance sequence is a disposable HTTPS login with separate password and TOTP steps, followed by a read-only confirmation that the configured operator can reach their intended console. Live validation records the authentication outcome without copying account data, credentials, cookies, or identity-provider responses.
