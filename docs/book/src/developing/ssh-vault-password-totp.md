# Scoped SSH password and TOTP authentication

## Configure an SSH authorization

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

The transport must run on Linux, Windows and macOS. The native transport keeps authentication and session reuse in the task
broker. Validate client-platform builds and execution separately.

The protocol adapter and the SSH loopback suite are available for review.
Run `cargo test --locked -p zeroclaw-tools --features portable-ssh --test portable_ssh_auth`
to exercise all six modes without a system SSH daemon. The test server sends
identical non-English prompt labels, validates the actual responses, and
covers host-key mismatch, visible fields and incomplete key-plus-TOTP.
The unified form writes an encrypted, revision-checked authorization. The
new task pool reloads the selected factor bindings before sending each new
command. The catalog and pool have been exercised against a disposable
QEMU OpenSSH server. The runtime exposes the authorization through the shell tool. Build with
`portable-ssh` and configure the daemon with `ZEROCLAW_SSH_CREDENTIAL_DIR`
(the encrypted catalog directory) and `ZEROCLAW_SSH_KNOWN_HOSTS` (the absolute
path to an operator-managed OpenSSH pin file). A missing, changed or ambiguous
pin denies the request. Keep one trusted key per endpoint in this file.

Use the shell tool with an SSH authorization instead of including credentials
in the remote command. For example:

```json
{
  "command": "hostname",
  "ssh_authorization": { "alias": "LAB_LOGIN", "host": "lab.example.net" }
}
```

The shell still requires its normal operator policy and approval. A sandboxed
shell cannot borrow the host's SSH authorization. An explicit username may
disambiguate multiple authorized accounts for the same key and endpoint.
The broker reuses the connection within one agent task, disconnects on
normal task completion, and closes an inactive transport after 30 minutes.
Verify the resulting runtime on each client platform before enabling it.

## Verification

- The Linux and Windows protocol tests exercise all six modes, including
  rejection of a wrong host pin and visible interactive fields.
- The runtime test against a disposable Ubuntu OpenSSH guest exercises
  password plus TOTP through the shell tool and encrypted catalog. It verifies
  connection reuse and denial after TOTP revocation or outside the task.
- The Chrome fixture exercises the unified form, both credential types,
  default and explicit ports, and a revision-checked single-destination
  revocation.

[Review evidence and platform status](https://github.com/zeroclaw-labs/zeroclaw/pull/12659).
