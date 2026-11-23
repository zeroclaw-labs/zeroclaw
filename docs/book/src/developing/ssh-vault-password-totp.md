# Scoped SSH password and TOTP authentication

## Revised authorization flow

The operator creates or edits this policy in Secrets → Usage permissions →
Add authorization. Selecting SSH reveals the destination list and a single
authentication mode. The form offers these modes:

| Credential alias | Authentication mode |
| --- | --- |
| SSH key | SSH key |
| SSH key | SSH key → TOTP |
| Username and password | Username, password |
| Username and password | Username, password → TOTP |
| Username and password | Username, TOTP → password |
| Username and password | Username, password + TOTP in one field |

For username/password credentials the authenticated SSH username and password
come from the selected alias. An explicit username in a destination must
match that credential's username. Key credentials instead use the authorized
destination's login. An omitted port means 22. Modes with TOTP require a
separate TOTP alias, scoped to the same authorization and exact destinations.

The mode controls authentication at the SSH protocol level. The username is
provided during SSH user authentication. It is never inferred from a
challenge. Interactive responses follow the selected sequence. No prompt
labels, language settings, or knowledge of the remote OS or authentication
stack are required. Each secret response requires an echo-disabled field,
and the transport rejects extra or missing fields before opening any command
channel. Key plus TOTP requires the server to acknowledge public-key partial
success before the client submits the TOTP. The transport verifies the
server's host key before using a credential and never downgrades the mode.

The transport must run on Linux, Windows and macOS. The existing Linux-only
OpenSSH helper cannot supply this portability or typed authentication state.
The implementation must keep authentication and session reuse in the native
task broker, with connection scope and task lifetime enforced consistently
on all supported systems. Verify the six modes against disposable SSH
servers and record client-platform build and runtime results separately.

The protocol adapter and the SSH loopback suite are available for review.
Run `cargo test --locked -p zeroclaw-tools --features portable-ssh --test portable_ssh_auth`
to exercise all six modes without a system SSH daemon. The test server sends
identical non-English prompt labels, validates the actual responses, and
covers host-key mismatch, visible fields and incomplete key-plus-TOTP.
The operator form and task-owned pool are separate integration work, and
Linux loopback results do not certify Windows or macOS.

The sections below describe the draft implementation that this design
supersedes. They document its current behavior until the new transport and
authorization form are implemented.


This proposal extends the task-owned native SSH broker from #12438. The target is
operator-managed SSH to a known server. The operator chooses one authentication mode per profile.

## Authentication modes

| Mode | Selected credentials | Authentication contract |
| --- | --- | --- |
| Public key (default) | SSH key and login | Existing public-key flow |
| Username and password | Username/password alias | Password authentication or one configured PAM password challenge |
| Username, password and TOTP | Username/password alias and TOTP alias | Both configured PAM challenges must succeed |

The password-only mode does not read a TOTP seed or answer one-time-code
prompts. A profile with TOTP must never downgrade to password-only when
a grant is missing, a vault is unavailable or the challenge fails. A username
identifies the SSH account. An SSH key or password still authenticates it.

## Authorization

The operator binds each credential independently to the exact SSH hostname,
port and login account. A password alias and a TOTP alias are separate grants.
For a profile requiring both, an operator selects both aliases. The agent may
request the profile but cannot choose a different pair, widen either grant or
see the stored password, seed or generated code.

Resolve the effective destination and check its trusted host key before answering
an authentication challenge. Reload every grant required by the selected mode after any vault lookup and refuse
authentication when either alias was deleted, rotated or revoked. Existing
task-owned connections must not accept new leases for a revoked profile.

## Authentication and process isolation

Support explicitly configured PAM challenge roles. Fail closed on unexpected
challenge types, ordering, retries or extra prompts. Restrict password and
code delivery to the broker-owned authentication exchange. Keep secret material
out of command arguments, environment values, output, traces, agent context
and persistent files. Generate the code close to the expected challenge and
bound the authentication deadline and attempt count.

The authenticated master retains the existing task scope, socket isolation and
idle expiration. Authentication material has a shorter lifetime than the
master. A cleanup failure must not result in fallback to an unmanaged SSH
connection.

## PAM challenge contract

The SSH login is fixed by the approved destination. If a keyboard-interactive
server also asks for a username, an operator-configured challenge can return
that same login. It cannot select another account.

Each keyboard-interactive profile declares an ordered set of expected challenge
roles and labels for that deployment. Matching ignores surrounding whitespace, case
and trailing colons, and accepts common English labels only for the same role.
Labels can be localized without
changing the broker. The supported roles are username, password, TOTP and an
explicit combined password-plus-code field. The combined role requires both
grants and an operator-selected format. A password-only profile cannot answer
a TOTP or combined challenge. The broker must match a challenge before looking
up the requested factor, and require the complete configured sequence before
making the authenticated session available.

The challenge tests must cover username, password, TOTP, separate password and
TOTP prompts, a combined field and a localized label. They must reject unknown,
ambiguous, repeated, reordered and unexpectedly omitted challenges. The string
received from the server never grants permission or changes the selected mode.

## WebUI and persisted SSH profiles

The operator manages SSH connection profiles from the credential configuration
WebUI. A form selects the destination, authentication mode and authorized
aliases. For PAM profiles it also edits the ordered challenge list, including
the role, accepted label and, for a combined field, the response format.
The UI lists only aliases authorized for the selected destination. It shows
secret aliases and grant metadata only.

The API validates and persists one canonical profile used by both the WebUI
and broker. The broker resolves the saved profile and verifies the grants
every time it admits an authentication or a new connection lease. The UI must
not present a mode as available until the configured broker supports it.

Acceptance requires creating and editing a profile in the WebUI, reloading
and verifying persistence, and using that same saved profile through the
broker against the QEMU guest. A second end-to-end check must revoke one
factor and verify that the broker refuses a subsequent lease.

## Validation required before deployment

Use a disposable QEMU guest with OpenSSH, PAM, a synthetic password and a
synthetic TOTP seed. Pin the guest host key. Publish the guest image identity,
the software versions and sanitized results for these checks:

- Test password-only and password-plus-TOTP against separate PAM policies
  in the QEMU guest. Each authorized profile runs a harmless marker command.
- Password-only never asks the vault for a TOTP seed and rejects an unexpected
  OTP prompt. Password-plus-TOTP fails if the second challenge is absent.
- The existing public-key mode still works with an explicit login and no TOTP.
- A missing or revoked grant prevents a new authentication or lease.
- A wrong or expired code is rejected without repeated attempts.
- A changed host key fails before a factor is released.
- Two commands reuse the same authorized task connection.
- Closing the task or reaching the idle deadline removes its sockets.
- The WebUI, logs and process metadata expose no factor values.

The native broker and the scoped credential catalog must both be present in
the integration base. The upstream broker branch alone does not include the
combined catalog currently deployed on Jarvis.

The Rust broker and operator UI implement direct SSH connections on Linux.
The operator configures the destination, account and authorized factor aliases
in Secrets → Usage permissions. PAM profiles match ordered challenge labels. The form offers password-only
and password-plus-TOTP presets with editable prompt labels.
Combined replies concatenate the password and current code.

A disposable QEMU guest verified password-only and password plus separate
PAM TOTP challenges, reuse of a single SSH connection, rejection of the wrong
account, denial of the next lease after removing the TOTP alias, and socket
cleanup at task exit. Combined and username challenge handling have unit
coverage and still need guest-level verification.
