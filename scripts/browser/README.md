# Private managed browser helper

This experimental helper serves the catalog browser consumer in #12596. It
uses a new headless Chromium process and a non-persistent context for each
tool-loop owner. It never attaches to the ordinary browser backend or exports
its context, page handles, cookies, storage, DOM, screenshots or debugging
endpoint. The Chromium sandbox remains enabled.

Install `npm ci` in this directory in an operator-controlled installation,
outside writable agent workspaces. Set `ZEROCLAW_MANAGED_BROWSER_HELPER` to the
absolute path of an executable launcher for `managed-browser.mjs`. On Windows,
use an operator-owned `.cmd` launcher invoking an absolute Node executable and
this script. On Linux, an executable script with the Node shebang can be used.
Node 24 or later is required. Configure `browser.native_chrome_path` with the
absolute Chromium executable, or install the Chromium version expected by
Playwright Core in its standard browser cache.

Keep the repository-relative layout containing this helper and
`crates/zeroclaw-tools/src/browser_credential_form.js`. The helper loads that
canonical form program from its trusted installation; the stdin protocol cannot
replace it with caller-supplied JavaScript. Missing assets fail closed.

The catalog's `browser.credential_submission_enabled` switch still defaults to
false. Its presence and origin grants remain independent admission checks. No
fallback to the ordinary agent-browser session is allowed when the helper is
missing, unsupported or fails.

## Operation and lifecycle

1. `browser(action="credential_open", url="https://login.example.test")`
   reserves a fresh private session for the current tool-loop invocation.
2. `submit_credential` resolves and revalidates the supplied alias, then sends
   username/password or a generated TOTP code over the private stdin pipe.
   Use the expected `credential_stage` for staged forms. Arbitrary field and
   frame selectors are refused in this path.
3. Generic browser actions are denied after reservation for the rest of that
   invocation, including after authentication or explicit close. This includes
   screenshots, snapshots, text extraction and computer-use capture. The private
   protocol has no arbitrary script, clipboard or session export command.
4. `credential_close`, completion, error, command timeout and owner cancellation
   close the input lease. The helper closes the context and browser on EOF,
   including while startup or navigation is pending. Normal termination awaits
   the helper's cleanup status. A dropped owner signals the independently owned
   supervisor to perform the same cleanup.

Nested/delegated tool-loop invocations receive a fresh scope. No caller-provided
task id or session name selects another scope. A steering yield ends the current
invocation; the private session is not restart-resumable.

## Deliberate restrictions

Only same-origin HTTPS navigation, resources and form POSTs are supported.
TOTP may use one visible or masked semantic OTP field, or a group of semantic
one-character OTP controls; no model-provided selector is needed.
Cross-origin redirects/resources, service workers, WebSockets, popups, downloads,
certificate errors and arbitrary selectors are refused. Only top-level standard
HTML forms and confirmed full-page navigations are supported by this first
private executor. A SPA, embedded login or external identity-provider redirect
may therefore be refused. No automatic retry follows an unconfirmed submission.
The result is a bounded acknowledgement, not proof of authentication.

The destination's own scripts necessarily receive form values and may observe
them. The boundary excludes the agent and other tasks, not the authorized site.
OS compromise, same-user access to trusted installations, forced process kills,
power loss and service-side logs are outside this browser-context guarantee.
This remains draft pending the RFC and parent merge decisions.

## Tests

`npm test` runs protocol/cleanup regressions. Set `ZEROCLAW_TEST_CHROMIUM` to a
Chromium executable to include the real-browser test. Its network fixture
intercepts HTTPS-shaped requests; it does not prove a real TLS exchange. Rust
tests cover private pipe filtering, distinct nested owners, error/cancellation
cleanup, encrypted catalog admission and lookup revocation.
