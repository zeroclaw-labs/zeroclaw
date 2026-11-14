# Task-owned SSH transports (Linux, opt-in)

This adapter reuses an OpenSSH connection for commands addressed to the same
configured alias and identical effective OpenSSH configuration during one logical
turn. Distinct aliases never share a master, even when their resolved endpoint is
the same. Concurrent calls use separate channels over a matching connection.
Streamed steering rounds retain the same owner and cached failures. The task owner
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
sockets and readiness state. Connection stderr is drained continuously into an
8 KiB in-memory tail, not an unbounded file. Connecting one key does not hold the
pool lock while waiting for the network; other keys and task shutdown proceed.

## Trust and credentials

### Existing SSH agent

The default mode uses existing SSH agent and public identity configuration. It
does not require any Python packages or vault CLI. Leave
`ZEROCLAW_SSH_DEFAULT_CREDENTIAL` unset to retain this mode by default.

### Scoped credential catalog

Scoped mode requires Python 3.11 or Python 3.10 with `tomli`, and the Python
`cryptography` package. Install them for the interpreter that runs the helper.
OpenBao credentials additionally require the `bao` CLI on the service PATH and
a service identity permitted to read the configured KV field over verified TLS.

1. Use Storage → Secrets to register an SSH key or an OpenBao KV field reference
   and authorize its resolved SSH hostname, port and login. Use strict known-host
   verification and provision the expected host key for the service account.
2. Set `ZEROCLAW_SSH_CREDENTIAL_DIR` in the daemon environment to its protected
   configuration directory containing `config.toml` and `.secret_key`.
3. Use `ssh --credential ALIAS HOST COMMAND` through the managed PATH wrapper.
   Optionally set `ZEROCLAW_SSH_DEFAULT_CREDENTIAL=ALIAS` to select it by default.

The helper checks the grant before each lease. A private signing socket exists
during master authentication and is retired before the first command can acquire
the transport. The helper does not write the SSH private key to disk and refuses
identity, trust, forwarding and multiplex overrides. The task's SSH masters close
when it ends, and a running OpenBao lookup is cancelled during shutdown.

The standalone Python helper uses gettext for its diagnostic messages. Optional
compiled `zeroclaw-ssh-task.mo` catalogs can be installed in the system locale
directory or under `ZEROCLAW_SSH_LOCALE_DIR` using standard gettext layout.
English is the fallback. The operator remains responsible for target authorization
and tool approvals.

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
