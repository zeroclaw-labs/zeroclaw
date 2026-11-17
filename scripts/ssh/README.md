# Task-owned SSH transports (Linux, opt-in)

This adapter reuses an OpenSSH connection for commands addressed to the same
configured alias and identical effective OpenSSH configuration during one logical
turn. Distinct aliases never share a master, even when their resolved endpoint is
the same. Concurrent calls use separate channels over a matching connection.
Streamed steering rounds retain the same owner and cached failures. The task owner
closes its helper pipe on success, error or cancellation; the helper closes all
masters. Masters also receive a Linux parent-death signal if the helper dies.
The helper forces foreground operation for both masters and command clients,
including when a host alias enables SSH backgrounding.

The idle watchdog defaults to 1800 seconds. It expires a connection only when
no command leases are active. A running command is not idle merely because it
does not produce output. After idle expiration a later command may establish a
new transport. An unexpected transport failure is instead cached for that task;
there is no automatic reconnect or unmanaged TCP fallback.

## Dependencies and setup

The Python adapter requires Linux, OpenSSH 8.7 or later, Python 3.10 or later, and util-linux `setpriv`
with `--pdeathsig`. Other platforms and existing installations are unchanged
when the helper is not configured. This initial implementation is for
noninteractive commands, not interactive logins, port forwarding or SCP.

1. Install `zeroclaw-ssh-task` in an operator-controlled, non-writable directory
   readable/executable by the ZeroClaw service account.
2. Set `ZEROCLAW_SSH_TASK_HELPER` to that executable's absolute path in the
   daemon's environment. Set `ZEROCLAW_SSH_IDLE_TIMEOUT_SECS=1800` or another
   interval between 1 and 86400 seconds there.
3. Provide a service-local PATH directory containing an `ssh` symlink to the
   helper, before the system SSH client. Ensure the shell tool's allowed PATH
   resolves to that directory. Existing host aliases and SSH agent configuration
   remain authoritative.
4. Restart the daemon and use the managed `ssh HOST COMMAND` command in shell
   tools. Absolute calls to `/usr/bin/ssh` bypass the adapter and must not be
   advertised by the operator's SSH skill. This is resource management, not an
   OS-level prohibition on all other SSH processes.

The runtime supplies `ZEROCLAW_SSH_TASK` and `ZEROCLAW_SSH_STATE_DIR` after
constructing the shell environment. Do not set them manually for agent tasks.
The helper uses a user-owned 0700 directory under XDG_RUNTIME_DIR or the private
temporary directory. No private keys are written there; it contains control
sockets and readiness state. The Python adapter drains connection stderr into an 8 KiB in-memory tail.
The native Rust broker currently discards OpenSSH stderr and reports localized
connection-stage errors. Connecting one key does not hold the
pool lock while waiting for the network; other keys and task shutdown proceed.

When a turn owns a host SSH broker, the built-in shell refuses commands under
an OS sandbox, including ordinary non-SSH commands. Unrelated turns without
a broker remain governed by the configured sandbox policy.

## Trust and credentials

The Python helper uses the operator-configured SSH agent. Catalog aliases and
the vault belong to the native broker. The Python helper needs only the
standard library, OpenSSH and the existing process supervision tools.

For localized helper diagnostics, install the optional compiled gettext
catalog for the service locale. Set `LANG` and, when using a custom catalog
directory, `ZEROCLAW_SSH_LOCALE_DIR`. OpenSSH and remote output follow their
respective process locales.

Use the existing SSH agent and configured public identity selection, including
an operator's vault-backed agent if present. The adapter does not read a vault,
copy a private key, change host-key trust, forward the agent or grant remote
permissions. Identity, trust, forwarding and multiplexing overrides are refused.
The operator remains responsible for target authorization and tool approvals.

Remote exit codes, stdin and stdout are preserved. A shell pipeline may still
mask its left-hand command's status; capture the SSH status before filtering
output, or explicitly use a compatible shell with pipefail.

## Validation and rollback

Run `python3 scripts/ssh/test_ssh_task.py` and
`cargo test -p zeroclaw-tools --lib task_ssh`. Integration checks should establish
two commands to one test host, verify a single master PID, then close the task's
stdin and verify every master exits. Repeat by killing the helper to check
parent-death cleanup. Use a short configured idle interval to test expiration
without waiting 30 minutes; verify an active command is not expired.

To disable, remove `ZEROCLAW_SSH_TASK_HELPER` and the PATH wrapper, then restart
the service. Do not remove existing SSH trust or credentials.

## Review boundary

This is an initial opt-in implementation for #11867. The helper and resource
owner live outside the transitional runtime crate. The small turn-entry and
shell-environment wiring requires maintainer ownership review, including any
bounded holding-crate exception required before merge. This proposal does not
grant itself such an exception or introduce a second task scheduler.

## Native broker preview

Build the Linux binary with `cargo build --locked --release -p zeroclaw-tools
--bin zeroclaw-ssh-task`. Configure the service helper path and the shell's
managed SSH wrapper to point to the same binary. The existing task protocol,
catalog, trust configuration, and idle timeout apply. Roll back by restoring
both paths to the previous helper. Keep the Python adapter during review.

To enable catalog-backed SSH credentials in the Rust broker:

1. Register an SSH key or OpenBao KV field in **Storage → Secrets** and grant
   the effective hostname, port and SSH login. Install the expected host key
   in the service user's configured known-hosts file.
2. Set `ZEROCLAW_SSH_CREDENTIAL_DIR` to the protected directory containing
   `config.toml` and its encryption key.
3. Invoke `ssh --credential ALIAS HOST COMMAND` through the managed wrapper,
   or set `ZEROCLAW_SSH_DEFAULT_CREDENTIAL` for a default alias. Keep the alias
   unset when rolling back to the agent-only Python helper.

The Rust broker reads the existing encrypted catalog with ZeroClaw's own
configuration and secret store. OpenSSH still owns transport and host-key
validation. Scoped credentials require OpenSSH 8.9 or newer. The signer
loads the key with a 20-second lifetime and a destination constraint derived
from the authorized effective user, hostname, port and host-key alias. It
uses the configured known-hosts files and refuses the scoped connection if
those files cannot establish the constraint. Use distinct, pinned host keys
for different servers. The SSH agent binds the constraint to host keys and
the username, while the catalog separately checks the effective port. The
broker terminates the signer before leasing the authenticated transport.

For OpenBao, the broker invokes the operator-managed `bao` CLI with discrete
arguments and a bounded lifetime and output. Its standard error is excluded
from user-facing diagnostics. Keep the CLI on the trusted service PATH and
configure service authentication and TLS through the CLI's normal settings.
Upgrading the CLI alongside OpenBao can accommodate protocol changes without
changing ZeroClaw, as long as the supported CLI commands and output contract
remain stable.

## Native broker security boundary

The native scoped signer requires OpenSSH 8.9 or newer. Broker, shell tools and
the private signer execute under the same operating-system user. Catalog keys
and the signer are therefore accessible to arbitrary unsandboxed shell code
running as that user. Destination-constrained signing is defense in depth.
Confidentiality against those shell processes requires a separately privileged
credential broker and is outside this adapter's current boundary.

Agent and master socket paths must be absent before setup. The broker rechecks
signer liveness after its socket appears and before loading a key. These checks
reject pre-bound fake sockets. They cannot eliminate a same-user replacement
race during authentication.

Each master owns a process group. Setup failure, cancellation, idle retirement
and task shutdown terminate the group's proxy descendants as well as the master.
