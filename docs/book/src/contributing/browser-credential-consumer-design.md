# Browser credential consumer design

This is a design for a follow-up to RFC #12422 and the extended credential catalog in #12517.

## Operator grants

In **Settings → Secrets → Permissions**, give username/password entries a browser-login grant and TOTP entries a browser-verification grant. Each grant defaults to denied and is scoped to an exact HTTPS origin, including its effective port. An operator can enable either grant or both independently. The HTTP Request grant stays independent. A destination change clears the affected browser grant. An unsupported backend shows that browser credential use is unavailable.

## Runtime boundary

The Permissions panel displays the scope warning before an operator adds or
edits a browser grant. This screenshot uses synthetic catalog metadata on the
production dashboard route, with the add-authorization editor open:

![Browser credential grant warning and authorization editor](../assets/screenshots/browser-credential-permissions.png)

The screenshot was captured from `0ea4ccae0` in Chromium at 1440 × 1000, in
English. The dashboard HTTP metadata was supplied by a disposable fixture.
The fixture also saved an additional browser origin, reloaded the page, and
confirmed that the separate TOTP entry retained its empty grant list. This
validates the operator interface, not managed session isolation or login.

Use an isolated managed browser context and trusted, purpose-built login operations. The caller supplies an alias. The executor resolves it just in time from the existing catalog and configured vault. Treat the seed as a secret and keep generated codes inside the executor. A password operation and a TOTP operation check their own grants, including after redirects and on multi-step login flows. A challenge displayed by a page carries no authority. Bind both the active top-level origin and the receiving document and submission destination to the approved HTTPS origin. Reject unsupported frames, insecure transport, certificate errors and ambiguous destinations. Retain existing browser policies and operator approvals. A delegated or resumed task must pass the same checks against current policy.

Keep credential-bearing pages and browser session material out of generic snapshots, screenshots, downloads, script evaluation, and tool results during the credential operation. Verify that secrets never enter process arguments, diagnostics, telemetry, model context, or persisted browser profiles. Start with a backend that can enforce these properties and fail closed elsewhere. Return only a bounded status. A successful fill is distinct from successful authentication. Support a TOTP-only grant for a session already at a verification step. Keep browser sessions isolated by task and close them on termination.

## Acceptance tests

Use disposable HTTPS fixtures. Cover password-only, TOTP-only, both grants and neither. Exercise same-page and staged challenges, revoked grants, changed destinations, cross-origin redirects and frames, backend refusal, cancellation, clock rollover, local and external vault resolution, and absence of credential material in every agent-visible output. Keep existing HTTP and SSH grants independent.

## Integration status

The draft implements separate catalog grants, current-record revalidation and a
private managed executor. The ordinary browser backend is never reused for
catalog release. The tool-loop owner creates a fresh private capability; nested
or delegated tool loops receive their own scope. The supervisor owns a helper's
stdin lease, and the helper owns a headless Chromium process and non-persistent
context. No model-selected task/session identifier can attach to it.

The private protocol exposes only opening an HTTPS page, obtaining its canonical
origin internally, and submitting a catalog-resolved form. Arbitrary scripts,
clipboard access, snapshots, screenshots, text extraction, cookie/storage export,
traces, video and downloads are absent. Generic browser actions are refused for
the rest of an invocation once it reserves a private context. The restriction
continues after authentication, explicit close or failed startup.

Only the authorized destination's scripts see its own form values. This does not
claim to hide credentials from that site or defend against OS compromise. The
browser sandbox remains enabled; no certificate-error bypass is installed.

The initial acceptance sequence is a disposable HTTPS login with separate password and TOTP steps, followed by a read-only confirmation that the configured operator can reach their intended console. Live validation records the authentication outcome without copying account data, credentials, cookies, or identity-provider responses.

## Draft runtime implementation

The explicit `browser.credential_submission_enabled` switch defaults to `false`,
independently of origin grants and browser automation enablement. Saving a grant
does not enable `submit_credential`. The executor reads this switch from the same
on-disk config as the catalog before any browser or vault access and again after
an awaited lookup. Missing, unreadable, malformed or disabled config refuses use.
This remains an experimental acceptance switch for disposable fixtures with
synthetic credentials while the RFC and parent merge decisions are pending.
Disable it to stop subsequent releases without deleting grants. Environment-only
overrides cannot enable this disk-authorized path.

Start with `credential_open` and an HTTPS URL. `submit_credential` then takes one
catalog alias on that private page. The executor rechecks the catalog and the
enablement switch after vault lookup, sending values through standard input.
The form helper accepts semantic visible top-level fields and a same-origin
HTTPS POST action. Its result indicates a confirmed navigation following
submission, not authentication. This implementation targets standard HTML forms
and six-digit SHA-1 TOTP with a 30-second step. See the
[helper setup and lifecycle contract](https://github.com/zeroclaw-labs/zeroclaw/blob/2bfd66e13d683cef5bdae8bb811b515ef0abad48/scripts/browser/README.md).

### Multi-step and embedded forms

Use `credential_stage` to select username, password, login, or otp. A username-only
call does not fetch the password. Passwords require a password input; TOTP can
use a visible or masked semantic OTP input or semantic one-character controls.
The private executor rejects model-provided `credential_field` and
`credential_frame` selectors, embedded forms, cross-origin redirects/resources
and unconfirmed SPA submissions. Each submission must POST to the same origin.
These restrictions reduce compatibility deliberately; no fallback to a generic
browser session is permitted.

Use `credential_close` to finish early. Completion, errors and cancellation end
the lease too, including during startup or navigation. Normal completion waits
for helper cleanup; dropping the owner signals the independent supervisor to
close the pipe and await the helper. The helper closes its context and browser
on EOF. A steering yield ends that invocation's context; it is not persisted or
restored on restart.

## Current limitations

This remains draft pending RFC #12422 and its catalog dependencies. The private
executor's isolation and observation restrictions are implemented and exercised
with synthetic browser fixtures; that is not independent acceptance against a
real external service. Origin grants authorize later password confirmations on
that origin, including sensitive operations. A refused or unconfirmed result may
follow a page-handled submission; do not automatically retry. No raw private-page
observation is exposed to resolve that uncertainty.

Every permission mutation requires the accepted catalog revision. Replacing a credential with a different suggested service clears HTTP and browser grants; rotation for the same service preserves them. A named OTP field must also match `autocomplete="one-time-code"` or `name="otp"`; a group must have one character per control.
