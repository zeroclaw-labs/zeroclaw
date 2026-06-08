# FreeBSD

ZeroClaw runs natively on FreeBSD (tested on FreeBSD 15.0-RELEASE, `amd64`). Two things differ from the Linux/macOS/Windows paths:

1. **No prebuilt binary and no `install.sh` support.** FreeBSD is not a target of the bootstrap installer, so you build from source with the system Rust toolchain.
2. **No `zeroclaw service` backend.** The `zeroclaw service install` command knows systemd, launchd, and Windows Task Scheduler, not FreeBSD `rc.d`. You install a small `rc.d` script yourself. This page gives you a complete, tested one.

Everything else, config, providers, channels, the daemon, the gateway, is identical to any other platform.

> **When to use FreeBSD.** FreeBSD deployments are common in network-appliance, embedded, and jail-based hosting where operators want the base system’s stability, ZFS + jail primitives, or need FreeBSD for policy/licensing reasons. Because there is no prebuilt binary and the `rc.d` setup is manual, this path suits operators comfortable with FreeBSD conventions. If you are only evaluating platforms with no specific FreeBSD requirement, Linux (systemd) or macOS (launchd) onboard faster via `install.sh` and `zeroclaw service install`.
>
> **Grab the files instead of copy-pasting.** Every shell script and sample config shown below ships in [`dist/freebsd/`](https://github.com/zeroclaw-labs/zeroclaw/tree/master/dist/freebsd): copy them to your host directly. The walkthrough explains what each piece does and why.

## System dependencies

Install the toolchain and runtime from `pkg`:

<div class="os-tabs-src">

#### sh

```sh
doas pkg install -y rust git
```

</div>

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

<div class="os-tabs-src">

#### sh

```sh
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo build --release
```

</div>

The release binary lands at `target/release/zeroclaw`. A clean build of the default feature set takes a while on modest hardware, this is expected; ZeroClaw is a large Rust workspace.

To trim the build, disable features you don't need (see `./install.sh --list-features` on a Linux box, or `Cargo.toml`):

<div class="os-tabs-src">

#### sh

```sh
cargo build --release --no-default-features --features agent-runtime
```

</div>

## Install the binary

Put it somewhere on `PATH`. `/usr/local/bin` is the conventional location for ports-installed binaries on FreeBSD:

<div class="os-tabs-src">

#### sh

```sh
doas install -m 755 target/release/zeroclaw /usr/local/bin/zeroclaw
zeroclaw --version
```

</div>

(`~/.cargo/bin/zeroclaw` works just as well if you'd rather keep it per-user.)

## First-run configuration

<div class="os-tabs-src">

#### sh

```sh
zeroclaw quickstart
```

</div>

This creates `~/.zeroclaw/` with a starter config and walks you through provider setup. Config layout and precedence are identical to every other platform: see [Reference → Config](../reference/config.md).

## Provider authentication

API-key providers need nothing FreeBSD-specific: set the key through the gateway, zerocode, `zeroclaw config set`, or the environment and you're done.

For OAuth-based providers (e.g. an OpenAI/Codex ChatGPT subscription), import the credential with:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw auth login --model-provider openai-codex --import /path/to/auth.json
zeroclaw auth status
```

</div>

> **Auth profiles are encrypted per-host and are *not* portable.** ZeroClaw stores resolved credentials in `~/.zeroclaw/auth-profiles.json`, encrypted with the host-local key at `~/.zeroclaw/.secret_key`. **Do not copy `auth-profiles.json` between machines**: the target host's `.secret_key` won't decrypt it and every request fails with `enc2: decryption failed`. Instead, copy the *raw* upstream credential (the un-encrypted `auth.json` your provider's own login produced) and re-run `zeroclaw auth login --import` on each host so it re-encrypts locally. If you hit a stale/undecryptable profile, move it aside (`mv ~/.zeroclaw/auth-profiles.json ~/.zeroclaw/auth-profiles.json.bak`) before re-importing.

## Running as a service (`rc.d`)

Because `zeroclaw service install` has no FreeBSD backend, supervise the daemon with FreeBSD's native [`daemon(8)`](https://man.freebsd.org/cgi/man.cgi?daemon%288%29) under an `rc.d` script. This gives you `service zeroclaw start|stop|restart|status`, restart-on-crash, a pidfile, and boot-time startup.

> **Ready-to-install copies of every script below live in [`dist/freebsd/`](https://github.com/zeroclaw-labs/zeroclaw/tree/master/dist/freebsd)** (`zeroclaw-run.sh`, the basic `zeroclaw.rc`, and the hardened `zeroclaw-hardened.rc`). The two `rc.d` scripts carry a `@@ZEROCLAW_USER@@` placeholder you `sed` in on install, so you can grab the files instead of copy-pasting: see `dist/freebsd/README.md`. The walkthrough below explains what each piece does.

### 1. Launcher script

`daemon(8)` starts the child with a minimal environment, so export a full `PATH` (FreeBSD puts `git`, `python3`, etc. under `/usr/local/bin`, which is *not* on the default service `PATH`). The `rc.d` script runs this through `daemon -u <user>`, so the launcher already executes as the service account: derive `HOME` from that account's passwd entry rather than hardcoding `/home/<user>`, so accounts whose home is elsewhere (and `rc.conf` run-as overrides) keep working. Save as `/usr/local/libexec/zeroclaw-run.sh`:

<div class="os-tabs-src">

#### sh

```sh
#!/bin/sh
: "${HOME:=$(eval echo ~)}"
export HOME
export PATH="/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin:${HOME}/bin"
exec /usr/local/bin/zeroclaw daemon --config-dir "${HOME}/.zeroclaw"
```

</div>

<div class="os-tabs-src">

#### sh

```sh
doas install -m 755 zeroclaw-run.sh /usr/local/libexec/zeroclaw-run.sh
```

</div>

### 2. `rc.d` script

Save as `/usr/local/etc/rc.d/zeroclaw`:

<div class="os-tabs-src">

#### sh

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
# Do NOT name this ${name}_user — rc.subr would then run its own su user-switch
# and collide with daemon -u ("failed to set user environment").
: ${zeroclaw_runas:="youruser"}

rundir="/var/run/zeroclaw"
pidfile="${rundir}/zeroclaw.pid"
logfile="/var/log/${name}.log"
launcher="/usr/local/libexec/zeroclaw-run.sh"

command="/usr/sbin/daemon"
command_args="-r -P ${pidfile} -o ${logfile} -u ${zeroclaw_runas} ${launcher}"

start_precmd="zeroclaw_precmd"

zeroclaw_precmd()
{
    # rundir + logfile stay root-owned: rc.d (root) writes the daemon -P pidfile
    # here and trusts it later, so the unprivileged service user must not be able
    # to forge it. daemon -o opens the logfile before dropping to ${zeroclaw_runas}.
    install -d -o root -g wheel -m 755 "${rundir}"
    install -o root -g wheel -m 640 /dev/null "${logfile}"
}

run_rc_command "$1"
```

</div>

<div class="os-tabs-src">

#### sh

```sh
doas install -m 755 zeroclaw /usr/local/etc/rc.d/zeroclaw
```

</div>

What the flags do:

- `-r`: supervise and restart the child if it exits (crash recovery).
- `-P ${pidfile}`: write the *supervisor's* pid so `service zeroclaw stop` can signal it.
- `-o ${logfile}`: redirect the child's stdout/stderr to a logfile.
- `-u ${zeroclaw_runas}`: run zeroclaw as an unprivileged user, not root.

> **Why `daemon -u` and not `su -m`.** A common pattern is `daemon ... su -m user -c launcher`. Avoid it: `su(1)` does **not** forward `SIGTERM` to its child, so `service zeroclaw stop` kills the `daemon` supervisor but leaves an orphaned `zeroclaw` process behind, and the next `start` stacks a second copy. `daemon -u user` makes `daemon(8)` the direct parent of `zeroclaw`, so it forwards the stop signal and shuts down cleanly. (If you're stuck with a `su`-based script for other reasons, add a `pkill -f "zeroclaw daemon"` sweep to its stop path.)

### 3. Enable and start

<div class="os-tabs-src">

#### sh

```sh
doas sysrc zeroclaw_enable=YES
doas sysrc zeroclaw_runas=youruser     # the account that owns ~/.zeroclaw

doas service zeroclaw start
doas service zeroclaw status
```

</div>

`service zeroclaw stop` / `restart` work as expected. Because `zeroclaw_enable=YES` is in `/etc/rc.conf` (written by `sysrc`), the daemon also starts on boot.

### 4. Hardening for unattended and remote operation

The script above is correct for an interactive, single-instance install. Three `daemon(8)` behaviours will surprise you the moment you drive the service remotely (over `ssh`) or run more than one copy. All three bit a production deployment; the fixes are small. A complete script folding in every fix below ships as [`dist/freebsd/zeroclaw-hardened.rc`](https://github.com/zeroclaw-labs/zeroclaw/tree/master/dist/freebsd): install it in place of the basic `zeroclaw` script.

**Remote `service ... start` hangs.** `daemon -r` inherits and holds open whatever stdin/stdout/stderr it was launched with. Run `ssh host 'service zeroclaw start'` and the supervisor keeps your `ssh` session's stdout fd open forever, so `ssh` never sees EOF and the command hangs even though the daemon started fine. Detach the supervisor's own descriptors: `-o ${logfile}` already routes the *child's* output, so nothing is lost:

<div class="os-tabs-src">

#### sh

```sh
command_args="-r -P ${pidfile} -o ${logfile} -u ${zeroclaw_runas} ${launcher}"
# ...invoke daemon with its own std{in,out,err} sent to /dev/null:
/usr/sbin/daemon ${command_args} </dev/null >/dev/null 2>&1
```

</div>

If you use the stock `command`/`command_args` form, wrap the start in a custom `start_cmd` so you control the redirection. This one change is what makes `service zeroclaw start` safe to call from `ssh`, CI, or a config-management push.

**Repeated `start` stacks orphan supervisors.** A plain `start` does not check whether a supervisor is already running, so a second `start` (or a `start` after a crash that left a stale pidfile) launches another `daemon` that fights the first over the gateway port. Make `start` idempotent by refusing when a live supervisor already exists. Match the supervisor by the launcher path, **not** the pidfile alone (the pidfile can be stale). Two FreeBSD-specific traps when you do this:

- `daemon(8)` *retitles its supervisor* to `daemon: /usr/local/libexec/zeroclaw-run.sh[<childpid>] (daemon)`. So `pgrep -f zeroclaw-run.sh` matches the supervisor, but a `pgrep -f` for the binary name does not. Bind on the literal `daemon:` prefix: that matches the supervisor and never the child, a hand-run of the launcher, or the rc shell itself. Bind the trailing `[` that opens daemon's `[<childpid>]` too, so a sibling launcher whose name merely *starts with* `zeroclaw-run.sh` can't match (this matters once you run a pool, see [Running a pool of instances](#4-hardening-for-unattended-and-remote-operation) below).
- FreeBSD `pgrep -f` does **not** honour a leading `^` anchor against that retitle string: `pgrep -f '^daemon: ...'` matches nothing. Drop the `^`; rely on the `daemon:` prefix for specificity and escape the dot in `.sh` as `[.]` (and the bracket as `[[]`) so they are literal.

<div class="os-tabs-src">

#### sh

```sh
launcher_pat="daemon: /usr/local/libexec/zeroclaw-run[.]sh[[]"

zeroclaw_running()
{
    pgrep -f "${launcher_pat}" >/dev/null 2>&1
}
```

</div>

**`read` from a `daemon -P` pidfile reports a false negative.** `daemon -P` writes the pid with **no trailing newline**, so `IFS= read -r pid < "${pidfile}"` returns a *non-zero* status (EOF before newline) even though it set `pid` correctly. If you guard it as `read -r pid < "$pf" || return 1`, every running instance looks stopped and your idempotent `start` happily launches a duplicate. Don't key the success path on `read`'s exit status: validate the value instead:

<div class="os-tabs-src">

#### sh

```sh
pid=""
IFS= read -r pid < "${pidfile}"     # do NOT `|| return 1` here
case "${pid}" in
    ''|*[!0-9]*) return 1 ;;        # empty or non-numeric → treat as not running
esac
```

</div>

**Running a pool of instances.** To run N daemons (e.g. a worker pool), give each its own pidfile and logfile (`worker.$i.pid`, `worker.$i.log`) and loop the start/stop over `$i`. Because the supervisor retitle is identical for every instance and does not include per-instance arguments, the **pidfile is the only per-instance handle**: drive stop/status from the pidfile, and on a full stop sweep any leftover supervisor that no live pidfile points at (started by hand, or whose pidfile went stale).

## Running in a jail

[Jails](https://docs.freebsd.org/en/books/handbook/jails/) give ZeroClaw an isolated root with its own packages, service user, and optionally its own IP, useful if the host runs other services or you want to constrain the agent. **The service setup is identical to the host case; you just run it *inside* the jail.** This walks through a classic thick jail with base-system tooling (no jail manager required).

> **One-step option.** [`dist/freebsd/zeroclaw-jail-setup.sh`](https://github.com/zeroclaw-labs/zeroclaw/tree/master/dist/freebsd) automates steps 1–3 below: it creates the jail, extracts a matching base, adds the `/etc/jail.conf` entry, starts the jail, and installs the launcher + hardened `rc.d` script inside it (`doas sh zeroclaw-jail-setup.sh`, with `JAIL_NAME` / `JAIL_PATH` / `ZPOOL` / `ZEROCLAW_USER` overridable via env). The manual walkthrough below explains what it does.

### 1. Create the jail

<div class="os-tabs-src">

#### sh

```sh
# ZFS dataset for the jail (use a plain directory if you're on UFS).
doas zfs create -o mountpoint=/jails/zeroclaw zroot/jails/zeroclaw   # adjust pool

# Extract a base matching the HOST's release into it.
doas fetch -o /tmp/base.txz \
    "https://download.freebsd.org/releases/$(uname -m)/$(freebsd-version -u)/base.txz"
doas tar -xpf /tmp/base.txz -C /jails/zeroclaw
doas cp /etc/resolv.conf /jails/zeroclaw/etc/
```

</div>

### 2. Configure and start

Add a jail entry to `/etc/jail.conf` (host side). This example shares the host network; set `ip4.addr` instead if you give the jail a dedicated address.

```
zeroclaw {
    host.hostname = "zeroclaw";
    path = "/jails/zeroclaw";
    exec.start = "/bin/sh /etc/rc";
    exec.stop  = "/bin/sh /etc/rc.shutdown";
    exec.clean;
    mount.devfs;
    persist;
}
```

<div class="os-tabs-src">

#### sh

```sh
doas sysrc jail_enable=YES
doas sysrc jail_list+=" zeroclaw"
doas service jail start zeroclaw
```

</div>

### 3. Install ZeroClaw inside the jail

Everything from the sections above runs *inside* the jail: prefix commands with `doas jexec zeroclaw …`, or open a shell with `doas jexec zeroclaw /bin/sh`:

<div class="os-tabs-src">

#### sh

```sh
doas jexec zeroclaw pkg install -y rust git     # or copy a binary built on the host
# build + install zeroclaw to /usr/local/bin/zeroclaw exactly as above, then:
doas jexec zeroclaw pw useradd zeroclaw -m -s /usr/sbin/nologin
```

</div>

Install the launcher and `rc.d` script into the **jail's** filesystem (from the host, the jail root is prefixed: `/jails/zeroclaw/usr/local/libexec/…` and `/jails/zeroclaw/usr/local/etc/rc.d/…`). Then enable and start the service *inside* the jail:

<div class="os-tabs-src">

#### sh

```sh
doas jexec zeroclaw sysrc zeroclaw_enable=YES
doas jexec zeroclaw service zeroclaw start
doas jexec zeroclaw service zeroclaw status
```

</div>

### Jail-specific notes

- **Edit jail files from the host with `tee`, not `cp /dev/stdin`.** Pipe through `… | doas tee /jails/zeroclaw/usr/local/etc/rc.d/zeroclaw >/dev/null`; `doas cp /dev/stdin …` can fail mid-copy with `cp: /dev/stdin: File changed`.
- **The gateway binds inside the jail.** The daemon listens on loopback by default: to reach it from the host or LAN, launch zeroclaw with `--host 0.0.0.0` (edit `zeroclaw-run.sh`) and give the jail a reachable address, or proxy from the host.
- **Prefer the hardened `rc.d` script in a jail.** You'll typically drive `service` non-interactively via `jexec`/`ssh`, which is exactly where the basic script's `start` hang and orphan-stacking bite: see [Hardening](#4-hardening-for-unattended-and-remote-operation). It also keeps `/var/run/zeroclaw` root-owned inside the jail so the unprivileged service user can't forge the supervisor pidfile.
- **Running several daemons in one jail** (e.g. a worker pool) follows the pool note in the hardening section: one pidfile/logfile per instance and a `pgrep` bound to the launcher retitle, since the jail shares one process table.

## Running the Linux image under Podman + Linuxulator

The native build above is the right path for ZeroClaw itself. But some Python-backed
tools and skills depend on **manylinux-only wheels**: `polars`, `pyarrow`, and
`oracledb`, for example, publish no FreeBSD wheels, so a tool that imports them can't
run under the native FreeBSD `python3`. FreeBSD's [Linuxulator](https://docs.freebsd.org/en/books/handbook/linuxemu/)
(Linux binary-compatibility layer) plus Podman lets you run the **official Linux
container image** on a FreeBSD host, giving those tools the Linux ABI they expect.
This complements the native `rc.d` daemon: you can run either, or both side by side.

### 1. Prerequisites

Load the Linuxulator modules and confirm they report a Linux release:

<div class="os-tabs-src">

#### sh

```sh
doas kldload linux linux64
sysctl compat.linux.osrelease        # e.g. compat.linux.osrelease: 5.15.0
```

</div>

To load them on boot, add `linux_enable="YES"` to `/etc/rc.conf`. Then install Podman:

<div class="os-tabs-src">

#### sh

```sh
doas pkg install -y podman
```

</div>

### 2. Pull the image: force the Linux platform

FreeBSD Podman defaults to `os=freebsd` when resolving a manifest list. ZeroClaw's
images are published only for `linux/amd64` and `linux/arm64`, so a plain `podman pull`
fails with `no image found in manifest list for architecture ..., OS freebsd`. Force
the Linux platform explicitly:

<div class="os-tabs-src">

#### sh

```sh
doas podman pull --os linux --arch amd64 ghcr.io/zeroclaw-labs/zeroclaw:debian
```

</div>

> Use the `debian` tag rather than `latest`: the distroless `latest` image has no
> shell, which makes it awkward to debug under emulation. See
> [Docker & Containers](./container.md) for the full image list.

### 3. Run the container

The Linux image behaves exactly as documented in [Docker & Containers](./container.md),
it expects persistent state at `/zeroclaw-data` and bootstraps a config on first run:

<div class="os-tabs-src">

#### sh

```sh
doas podman run -d --name zeroclaw --restart=always \
    --os linux --arch amd64 \
    -p 42617:42617 \
    -v /var/db/zeroclaw:/zeroclaw-data \
    ghcr.io/zeroclaw-labs/zeroclaw:debian

doas podman exec -it zeroclaw zeroclaw quickstart
```

</div>

Keep the `--os linux --arch amd64` flags on every `run` (not just `pull`) so Podman
doesn't re-resolve to the FreeBSD default.

### Linuxulator notes

- **Boot persistence.** Podman's own `--restart=always` only restarts the container
  within a running Podman; it won't survive a host reboot on its own. Supervise the
  `podman start` from an `rc.d` script (same pattern as the [native service](#running-as-a-service-rcd))
  or a `@reboot` cron entry so the container comes back after the host restarts.
- **Networking.** `-p 42617:42617` publishes the gateway through Podman's bridge. If
  the bridge/CNI setup isn't configured on your host, `--network host` is the simplest
  alternative: the container then shares the host's network stack directly.
- **Not everything emulates cleanly.** Linuxulator covers the common syscall surface,
  but exotic binaries may hit unimplemented calls. If a tool misbehaves, check
  `dmesg` for `linux:` warnings before assuming a ZeroClaw bug.

## Logs

<div class="os-tabs-src">

#### sh

```sh
tail -f /var/log/zeroclaw.log
```

</div>

Set the log level via the standard config / env knobs: see [Operations → Logs & observability](../ops/observability.md).

## Verify

<div class="os-tabs-src">

#### sh

```sh
zeroclaw --version
service zeroclaw status
# if the daemon exposes the local gateway (default 127.0.0.1:42617):
fetch -qo - http://127.0.0.1:42617/health
```

</div>

A `"status":"ok"` health payload means the gateway, daemon, and channels came up.

## Uninstall

<div class="os-tabs-src">

#### sh

```sh
doas service zeroclaw stop
doas sysrc -x zeroclaw_enable
doas rm /usr/local/etc/rc.d/zeroclaw /usr/local/libexec/zeroclaw-run.sh
doas rm /usr/local/bin/zeroclaw
rm -rf ~/.zeroclaw        # optional — deletes config + history
```

</div>

## Next

- [Service management](./service.md): how the first-party backends work on other platforms
- [Reference → Config](../reference/config.md): config file layout
- [Quickstart](../getting-started/quickstart.md): first conversation
- [Operations → Overview](../ops/overview.md): running in production
