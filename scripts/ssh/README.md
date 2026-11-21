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
an OS sandbox. Shell and coding CLI launchers reject sandboxed execution
while the host helper is configured or a broker lease is active. This also
protects a broker owned by a different turn when the sandbox shares host paths.

Foreground parallel delegates awaited by the current turn reuse its SSH task
owner. Tokio workers restore the captured task identity before entering their
loop, so they do not silently start another helper with a different agent.
The parent retains the lease and closes it at the task boundary. Detached
background jobs do not inherit this owner and keep their own lifetime.

## Trust and credentials

The Python helper uses the operator-configured SSH agent. Neither executable reads catalog credentials or loads keys from OpenBao. The Python helper needs only the standard library, OpenSSH and the existing process supervision tools.

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
SSH-agent identity, trust configuration, and idle timeout apply. Roll back by restoring
both paths to the previous helper. For rollback, retain a protected backup of the previous helper executable and PATH link.

## Native broker scope

The Rust broker uses the operator-configured SSH agent and existing OpenSSH
host aliases. It requires Linux, OpenSSH 8.7 or later, and util-linux setpriv. Configure the
native executable using the helper path described above.

Each persistent master owns a process group. Connection setup failure,
cancellation, idle retirement and task shutdown clean up proxy descendants.
Pre-existing master socket paths are refused. The broker's directory is private
to its operating-system user. Arbitrary unsandboxed code under the same user
has that user's SSH authority.

Catalog-backed credential loading and broker-managed vault signing are not supported by this executable.

### Abrupt native broker death

Each native master has a separate Rust supervisor in its own process group.
The broker retains the supervisor's stdin write end as a lifetime lease; only
that broker owns it. Normal retirement or broker death closes the pipe. The
supervisor keeps running long enough to send TERM and then KILL after 150 ms
to its complete group, including a proxy that ignores TERM and closed pipes.
It also cleans the group when the SSH leader exits. The group must equal the
supervisor PID and exceed 1; the internal mode refuses a shared group or a
broadcast PID. Its command shape is restricted to the managed setpriv/SSH
master invocation. There is no new credential lookup, listener or scheduler.
This adds one supervisor process per master. Processes that deliberately leave
the owned group are outside this process-group cleanup contract; unsandboxed
same-user code retains that user's authority. Existing task, idle and policy
limits continue to apply.

The regression kills an isolated broker fixture with SIGKILL while a real
TERM-ignoring proxy remains alive, then verifies both proxy and supervisor
terminate within the bounded deadline. Normal EOF and idle cleanup remain
covered separately. No Python supervisor is used by the native path.
