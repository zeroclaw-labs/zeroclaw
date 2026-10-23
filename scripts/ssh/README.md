# Task-owned SSH transports (Linux, opt-in)

This adapter reuses an OpenSSH connection for commands addressed to the same
resolved login, host, port, proxy and agent route during one tool-loop task.
Concurrent calls use separate channels over that connection. The task owner
closes its helper pipe on success, error or cancellation; the helper closes all
masters. Masters also receive a Linux parent-death signal if the helper dies.

The idle watchdog defaults to 1800 seconds. It expires a connection only when
no command leases are active. A running command is not idle merely because it
does not produce output. After idle expiration a later command may establish a
new transport. An unexpected transport failure is instead cached for that task;
there is no automatic reconnect or unmanaged TCP fallback.

## Dependencies and setup

Requires Linux, OpenSSH client, Python 3.10 or later, and util-linux `setpriv`
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
sockets, readiness state and bounded connection error logs.

## Trust and credentials

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
