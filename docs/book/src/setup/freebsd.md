# FreeBSD

Install ZeroClaw on FreeBSD from the ports tree or as a package.

## Install

ZeroClaw ships as a FreeBSD `USES=cargo` port, `misc/zeroclaw`. It installs
`zeroclaw` (the daemon/CLI), `zeroclaw-acp-bridge`, and `zerocode` (the terminal
config manager — see [zerocode](../getting-started/tui.md)).

### Option 1 — pkg (once available in the official tree)

```sh
pkg install zeroclaw
```

The port has been submitted upstream
([bug #295837](https://bugs.freebsd.org/bugzilla/show_bug.cgi?id=295837)). Until
it lands in the package set, build from the port (Option 2) or the in-tree
overlay (Option 3).

### Option 2 — from a ports tree

```sh
cd /usr/ports/misc/zeroclaw
make install clean
```

### Option 3 — from the in-tree overlay

The port recipe is carried in this repository under `dist/freebsd/`, alongside
the other downstream packaging targets (`dist/aur`, `dist/scoop`):

```sh
cp -R dist/freebsd/misc/zeroclaw /usr/ports/misc/zeroclaw
cd /usr/ports/misc/zeroclaw && make install clean

# or point poudriere at it as an overlay
poudriere testport -o misc/zeroclaw -M /path/to/zeroclaw/dist/freebsd
```

Once installed, see [Quick start](../getting-started/quick-start.md) to
configure your first agent.

## Build requirements

Declared as `BUILD_DEPENDS` and pulled automatically by the ports framework:
`lang/rust` (the framework's `RUST_DEFAULT`, set by `cargo.mk` — not the
project MSRV), `devel/cmake-core`, and `devel/pkgconf`. If a quarterly pkg
branch carries an older `lang/rust` than the cargo framework requires, install
a newer one from the `latest` branch.

## Channels

`pkg install zeroclaw` is a prebuilt binary — it is already compiled with the
workspace's full-channel feature set, so enable any channel in your config and
it works without recompiling. See
[Channels & Integrations](../channels/overview.md).

Feature selection is a from-source choice only (a prebuilt package cannot take
feature flags). Building from the port, override `CARGO_FEATURES`:

```sh
make CARGO_FEATURES="..." install clean
```

Run `zeroclaw --list-features` for the available features; the port keeps no
list of its own.

## Running as a service

ZeroClaw's built-in `service` subcommand targets systemd, launchd, and OpenRC —
it does not generate a FreeBSD `rc.d` script. On FreeBSD, run `zeroclaw daemon`
directly, or write a standard `rc.d` script under `${PREFIX}/etc/rc.d/` that
launches it. See [Service management](./service.md) for the cross-platform
service model and where the workspace lives.

## Uninstall

```sh
pkg delete zeroclaw      # or: cd /usr/ports/misc/zeroclaw && make deinstall
```
