# Scoped SSH password and TOTP authentication

This proposal extends the task-owned native SSH broker from #12438. The target is
operator-managed SSH to a known server that requires an account password and
a PAM one-time code. The public-key path remains the default.

## Authorization

The operator binds each credential independently to the exact SSH hostname,
port and login account. A password alias and a TOTP alias are separate grants.
For a profile requiring both, an operator selects both aliases. The agent may
request the profile but cannot choose a different pair, widen either grant or
see the stored password, seed or generated code.

Resolve the effective destination and check its trusted host key before answering
an authentication challenge. Reload both grants after any vault lookup and refuse
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

- The authorized profile authenticates and runs a harmless marker command.
- A missing or revoked grant prevents a new authentication or lease.
- A wrong or expired code is rejected without repeated attempts.
- A changed host key fails before a factor is released.
- Two commands reuse the same authorized task connection.
- Closing the task or reaching the idle deadline removes its sockets.
- The WebUI, logs and process metadata expose no factor values.

This document records the proposed contract. Runtime implementation and
guest verification are required before the feature can be enabled.
