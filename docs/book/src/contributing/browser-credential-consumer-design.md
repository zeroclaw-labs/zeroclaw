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

## Agent-browser transport probe

A disposable browser session on Jarvis, run as the ZeroClaw service user, verified that agent-browser 0.39.1 and its installed Chrome can run with a private temporary socket directory and execute a synthetic expression supplied through standard input. The result was 42. The probe did not use a vault record, a live identity provider, or the running service's browser session.

A one-shot credential-provider plugin is another supported input path for username/password. Neither the stdin probe nor the plugin API provides the full TOTP and authenticated-session contract by itself. The managed executor must bind release to the correct document, prevent generic browser observations while credentials are present, discard raw backend errors, and close the disposable session on failure or cancellation.

## Draft runtime implementation

The action submit_credential takes one catalog alias on the active login page. The executor rechecks the catalog after vault lookup and sends values through standard input. The form helper accepts visible top-level fields and a same-origin HTTPS POST action. Its result indicates submission, not authentication. This implementation targets standard HTML forms and six-digit SHA-1 TOTP with a 30-second step.


## Staged HTTPS browser validation

A loopback-only HTTPS fixture on Jarvis used a one-time certificate pinned to its SPKI in a disposable Chrome session. As the ZeroClaw service user, the test invoked the exact form helper through agent-browser stdin. Its server accepted the synthetic username/password POST, returned a separate TOTP form, accepted the synthetic code POST, and returned the authenticated page title. The fixture exposed neither credential values nor the submitted body in its logs. The probe reported HTTPS_PASSWORD_AND_TOTP_OK and stopped the fixture and browser session. This verifies browser transport and the form helper. A full agent run against an external identity provider requires separate acceptance.

## Authorization list

Use Secrets → Permissions → Add authorization. Choose the catalog alias, a compatible consumer, and the destination. The saved authorization appears in the list. Edit or remove any authorization independently of other grants and the underlying credential. The alias identifies the secret. The destination shown when adding a grant is only a suggestion. Browser and HTTP Request grants each support multiple explicit, exact HTTPS origins for their compatible credential types. Changing the suggested service does not grant access or retarget a saved authorization. SSH grants use username, hostname and port. Plugin TLS credentials are selected through a plugin's TLS profile configuration.

## End-to-end acceptance

The prompt supplies the site, the username/password alias, the optional TOTP alias, and the task to perform after login. The agent passes each alias to the browser tool when the corresponding form is present. The tool resolves the alias from the active catalog, checks the current grant for that exact origin, and retrieves the configured credential from its selected store. The executor does not infer permission from a prompt or page content.

Verify that the agent completes the password and optional TOTP steps, reaches the authenticated page, and reports only data observed there. A submission acknowledgement is not proof of authentication. If the site requires an access check before exposing a login form, record the stage reached and resume only after normal verification in the same session. Record provider-specific results in the PR's acceptance evidence.

### Multi-step and embedded forms

Use credential_stage to select username, password, login, or otp. A username-only call does not fetch the password. Use credential_field for a field observed on the page, or for a group of one-character OTP controls. Use credential_frame for a single visible iframe when its document and the top-level page share the authorized HTTPS origin. Each submission still requires the form to send a POST to that origin. A cross-origin login embedded by a site must be opened through its normal sign-in navigation before the tool can use a grant for that other origin. Provider access checks remain separate from credential submission.
