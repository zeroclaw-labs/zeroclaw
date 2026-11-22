# Scoped SSH password and TOTP authentication

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

This document records the proposed contract. Runtime implementation and
guest verification are required before the feature can be enabled.
