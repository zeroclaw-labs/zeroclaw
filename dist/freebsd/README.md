# zeroclaw — FreeBSD port

A FreeBSD `USES=cargo` port for [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw),
carried in-tree under `dist/freebsd/` alongside the other downstream packaging
targets (`dist/aur`, `dist/scoop`).

## Layout

```
misc/zeroclaw/   the authored port (Makefile, pkg-descr)
```

`distinfo` and the crate list (`CARGO_CRATES`) are **not** in this directory.
They are generated from the project's `Cargo.lock` on a FreeBSD host and are
git-ignored — committing them would mean hand-maintaining a 1000+ line
dependency list on every release, which is exactly what the framework generates
for you.

## Building

On a FreeBSD host with the ports framework and the cargo toolchain:

```sh
cp -R dist/freebsd/misc/zeroclaw /usr/ports/misc/zeroclaw
cd /usr/ports/misc/zeroclaw
make cargo-crates-merge     # Cargo.lock -> CARGO_CRATES + distinfo
make install clean
```

`make cargo-crates-merge` reads the extracted source's `Cargo.lock`, writes the
crate list via `portedit`, and regenerates `distinfo`. It preserves the
`@git+` entry for the `whatsapp-web` channel's git workspace
(`oxidezap/whatsapp-rust`).

Installs `bin/zeroclaw`, `bin/zeroclaw-acp-bridge`, and `bin/zerocode`.

## Channels

`pkg install` ships a prebuilt binary compiled with the workspace's
full-channel feature alias (cargo resolves its members from `Cargo.toml`), so
every channel is present. Feature selection is a from-source choice only — a
prebuilt package takes no feature flags. A source build can override
`CARGO_FEATURES`; run `zeroclaw --list-features` for the list.

## Updating / publishing

On a stable release the `pub-freebsd.yml` workflow regenerates `distinfo` +
`CARGO_CRATES` from the tag's `Cargo.lock` in a FreeBSD VM (real ports
framework — `make cargo-crates-merge`), bumps `DISTVERSION`, runs
`portlint`/`portfmt`, and uploads the bumped port as a release artifact. FreeBSD
ports submission is maintainer-driven via bugzilla, so the workflow stops at the
artifact rather than auto-pushing like AUR/Scoop.

To regenerate locally on a FreeBSD host:

```sh
cd /usr/ports/misc/zeroclaw
make cargo-crates-merge
```

## License

The port follows ZeroClaw's dual MIT / Apache-2.0 license.

## Upstream

Submitted to the FreeBSD ports tree:
**[bug #295837](https://bugs.freebsd.org/bugzilla/show_bug.cgi?id=295837)**.
Maintainer: jperlow@gmail.com.
