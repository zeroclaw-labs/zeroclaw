# FreeBSD

ZeroClaw runs natively on FreeBSD (tested on FreeBSD 15.0-RELEASE, `amd64`). Two things differ from the Linux/macOS/Windows paths:

1. **No prebuilt binary and no `install.sh` support.** FreeBSD is not a target of the bootstrap installer, so you build from source with the system Rust toolchain.
2. **No `zeroclaw service` backend.** The `zeroclaw service install` command knows systemd, launchd, and Windows Task Scheduler — not FreeBSD `rc.d`. You install a small `rc.d` script yourself. This page gives you a complete, tested one.

Everything else — config, providers, channels, the daemon, the gateway — is identical to any other platform.

## System dependencies

Install the toolchain and runtime from `pkg`:

```sh
doas pkg install -y rust git
```

| Package | Why |
|---|---|
| `rust` | Provides `cargo` and `rustc` to build the binary. The port tracks a recent stable (1.94+ at time of writing). |
| `git` | Cloning the repo, and required at runtime if you use any git-backed tools. |

> **`doas`, not `sudo`.** FreeBSD ships `doas` as the base privilege-escalation tool; `sudo` is an optional port. The examples here use `doas`. A minimal `/usr/local/etc/doas.conf` granting the `wheel` group passwordless escalation is:
>
> ```
> permit nopass keepenv :wheel
> ```

## Build from source

```sh
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo build --release
```

The release binary lands at `target/release/zeroclaw`. A clean build of the default feature set takes a while on modest hardware — this is expected; ZeroClaw is a large Rust workspace.

To trim the build, disable features you don't need (see `./install.sh --list-features` on a Linux box, or `Cargo.toml`):

```sh
cargo build --release --no-default-features --features agent-runtime
```

## Install the binary

Put it somewhere on `PATH`. `/usr/local/bin` is the conventional location for ports-installed binaries on FreeBSD:

```sh
doas install -m 755 target/release/zeroclaw /usr/local/bin/zeroclaw
zeroclaw --version
```

(`~/.cargo/bin/zeroclaw` works just as well if you'd rather keep it per-user.)

## First-run configuration

```sh
zeroclaw onboard
```

This creates `~/.zeroclaw/` with a starter `config.toml` and walks you through provider setup. Config layout and precedence are identical to every other platform — see [Reference → Config](../reference/config.md).

## Provider authentication

API-key providers need nothing FreeBSD-specific — set the key in `config.toml` or the environment and you're done.

For OAuth-based providers (e.g. an OpenAI/Codex ChatGPT subscription), import the credential with:

```sh
zeroclaw auth login --model-provider openai-codex --import /path/to/auth.json
zeroclaw auth status
```

> **Auth profiles are encrypted per-host and are *not* portable.** ZeroClaw stores resolved credentials in `~/.zeroclaw/auth-profiles.json`, encrypted with the host-local key at `~/.zeroclaw/.secret_key`. **Do not copy `auth-profiles.json` between machines** — the target host's `.secret_key` won't decrypt it and every request fails with `enc2: decryption failed`. Instead, copy the *raw* upstream credential (the un-encrypted `auth.json` your provider's own login produced) and re-run `zeroclaw auth login --import` on each host so it re-encrypts locally. If you hit a stale/undecryptable profile, move it aside (`mv ~/.zeroclaw/auth-profiles.json ~/.zeroclaw/auth-profiles.json.bak`) before re-importing.

## Running as a service (`rc.d`)

Because `zeroclaw service install` has no FreeBSD backend, supervise the daemon with FreeBSD's native [`daemon(8)`](https://man.freebsd.org/cgi/man.cgi?daemon%288%29) under an `rc.d` script. This gives you `service zeroclaw start|stop|restart|status`, restart-on-crash, a pidfile, and boot-time startup.

### 1. Launcher script

`daemon(8)` starts the child with a minimal environment, so export `HOME` and a full `PATH` (FreeBSD puts `git`, `python3`, etc. under `/usr/local/bin`, which is *not* on the default service `PATH`). Save as `/usr/local/libexec/zeroclaw-run.sh`:

```sh
#!/bin/sh
export HOME=/home/youruser
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin:/home/youruser/bin
exec /usr/local/bin/zeroclaw daemon --config-dir "$HOME/.zeroclaw"
```

```sh
doas install -m 755 zeroclaw-run.sh /usr/local/libexec/zeroclaw-run.sh
```

### 2. `rc.d` script

Save as `/usr/local/etc/rc.d/zeroclaw`:

```sh
#!/bin/sh
#
# PROVIDE: zeroclaw
# REQUIRE: NETWORKING DAEMON
# KEYWORD: shutdown

. /etc/rc.subr

name="zeroclaw"
rcvar="zeroclaw_enable"

load_rc_config $name

: ${zeroclaw_enable:="NO"}
: ${zeroclaw_user:="youruser"}

rundir="/var/run/zeroclaw"
pidfile="${rundir}/zeroclaw.pid"
logfile="/var/log/${name}.log"
launcher="/usr/local/libexec/zeroclaw-run.sh"

command="/usr/sbin/daemon"
command_args="-r -P ${pidfile} -o ${logfile} -u ${zeroclaw_user} ${launcher}"

start_precmd="zeroclaw_precmd"

zeroclaw_precmd()
{
    install -d -o ${zeroclaw_user} -g wheel -m 755 "${rundir}"
    install -o ${zeroclaw_user} -m 640 /dev/null "${logfile}"
}

run_rc_command "$1"
```

```sh
doas install -m 755 zeroclaw /usr/local/etc/rc.d/zeroclaw
```

What the flags do:

- `-r` — supervise and restart the child if it exits (crash recovery).
- `-P ${pidfile}` — write the *supervisor's* pid so `service zeroclaw stop` can signal it.
- `-o ${logfile}` — redirect the child's stdout/stderr to a logfile.
- `-u ${zeroclaw_user}` — run zeroclaw as an unprivileged user, not root.

> **Why `daemon -u` and not `su -m`.** A common pattern is `daemon ... su -m user -c launcher`. Avoid it: `su(1)` does **not** forward `SIGTERM` to its child, so `service zeroclaw stop` kills the `daemon` supervisor but leaves an orphaned `zeroclaw` process behind — and the next `start` stacks a second copy. `daemon -u user` makes `daemon(8)` the direct parent of `zeroclaw`, so it forwards the stop signal and shuts down cleanly. (If you're stuck with a `su`-based script for other reasons, add a `pkill -f "zeroclaw daemon"` sweep to its stop path.)

### 3. Enable and start

```sh
doas sysrc zeroclaw_enable=YES
doas sysrc zeroclaw_user=youruser      # the account that owns ~/.zeroclaw

doas service zeroclaw start
doas service zeroclaw status
```

`service zeroclaw stop` / `restart` work as expected. Because `zeroclaw_enable=YES` is in `/etc/rc.conf` (written by `sysrc`), the daemon also starts on boot.

### 4. Hardening for unattended and remote operation

The script above is correct for an interactive, single-instance install. Three `daemon(8)` behaviours will surprise you the moment you drive the service remotely (over `ssh`) or run more than one copy. All three bit a production deployment; the fixes are small.

**Remote `service ... start` hangs.** `daemon -r` inherits and holds open whatever stdin/stdout/stderr it was launched with. Run `ssh host 'service zeroclaw start'` and the supervisor keeps your `ssh` session's stdout fd open forever, so `ssh` never sees EOF and the command hangs even though the daemon started fine. Detach the supervisor's own descriptors — `-o ${logfile}` already routes the *child's* output, so nothing is lost:

```sh
command_args="-r -P ${pidfile} -o ${logfile} -u ${zeroclaw_user} ${launcher}"
# ...invoke daemon with its own std{in,out,err} sent to /dev/null:
/usr/sbin/daemon ${command_args} </dev/null >/dev/null 2>&1
```

If you use the stock `command`/`command_args` form, wrap the start in a custom `start_cmd` so you control the redirection. This one change is what makes `service zeroclaw start` safe to call from `ssh`, CI, or a config-management push.

**Repeated `start` stacks orphan supervisors.** A plain `start` does not check whether a supervisor is already running, so a second `start` (or a `start` after a crash that left a stale pidfile) launches another `daemon` that fights the first over the gateway port. Make `start` idempotent by refusing when a live supervisor already exists. Match the supervisor by the launcher path, **not** the pidfile alone (the pidfile can be stale). Two FreeBSD-specific traps when you do this:

- `daemon(8)` *retitles its supervisor* to `daemon: /usr/local/libexec/zeroclaw-run.sh[<childpid>] (daemon)`. So `pgrep -f zeroclaw-run.sh` matches the supervisor, but a `pgrep -f` for the binary name does not. Bind on the literal `daemon: ` prefix — that matches the supervisor and never the child, a hand-run of the launcher, or the rc shell itself.
- FreeBSD `pgrep -f` does **not** honour a leading `^` anchor against that retitle string — `pgrep -f '^daemon: ...'` matches nothing. Drop the `^`; rely on the `daemon: ` prefix for specificity and escape the dot in `.sh` as `[.]` so it is literal.

```sh
launcher_pat="daemon: /usr/local/libexec/zeroclaw-run[.]sh"

zeroclaw_running()
{
    pgrep -f "${launcher_pat}" >/dev/null 2>&1
}
```

**`read` from a `daemon -P` pidfile reports a false negative.** `daemon -P` writes the pid with **no trailing newline**, so `IFS= read -r pid < "${pidfile}"` returns a *non-zero* status (EOF before newline) even though it set `pid` correctly. If you guard it as `read -r pid < "$pf" || return 1`, every running instance looks stopped and your idempotent `start` happily launches a duplicate. Don't key the success path on `read`'s exit status — validate the value instead:

```sh
pid=""
IFS= read -r pid < "${pidfile}"     # do NOT `|| return 1` here
case "${pid}" in
    ''|*[!0-9]*) return 1 ;;        # empty or non-numeric → treat as not running
esac
```

**Running a pool of instances.** To run N daemons (e.g. a worker pool), give each its own pidfile and logfile (`worker.$i.pid`, `worker.$i.log`) and loop the start/stop over `$i`. Because the supervisor retitle is identical for every instance and does not include per-instance arguments, the **pidfile is the only per-instance handle** — drive stop/status from the pidfile, and on a full stop sweep any leftover supervisor that no live pidfile points at (started by hand, or whose pidfile went stale).

## Logs

```sh
tail -f /var/log/zeroclaw.log
```

Set the log level via the standard config / env knobs — see [Operations → Logs & observability](../ops/observability.md).

## Verify

```sh
zeroclaw --version
service zeroclaw status
# if the daemon exposes the local gateway (default 127.0.0.1:42617):
fetch -qo - http://127.0.0.1:42617/health
```

A `"status":"ok"` health payload means the gateway, daemon, and channels came up.

## Uninstall

```sh
doas service zeroclaw stop
doas sysrc -x zeroclaw_enable
doas rm /usr/local/etc/rc.d/zeroclaw /usr/local/libexec/zeroclaw-run.sh
doas rm /usr/local/bin/zeroclaw
rm -rf ~/.zeroclaw        # optional — deletes config + history
```

## Next

- [Service management](./service.md) — how the first-party backends work on other platforms
- [Reference → Config](../reference/config.md) — config file layout
- [Quick start](../getting-started/quick-start.md) — first conversation
- [Operations → Overview](../ops/overview.md) — running in production
