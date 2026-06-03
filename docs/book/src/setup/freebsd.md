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
