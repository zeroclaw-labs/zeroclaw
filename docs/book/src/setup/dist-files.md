# Platform-specific install files

ZeroClaw ships ready-to-use packaging and service files for several platforms under [`dist/`](https://github.com/zeroclaw-labs/zeroclaw/tree/master/dist). These are operator-facing — copy them to your host instead of hand-writing package or service definitions.

| Platform | Directory | Contents |
|---|---|---|
| Arch Linux | [`dist/aur/`](https://github.com/zeroclaw-labs/zeroclaw/tree/master/dist/aur) | `PKGBUILD` for the AUR package. |
| Windows | [`dist/scoop/`](https://github.com/zeroclaw-labs/zeroclaw/tree/master/dist/scoop) | `zeroclaw.json` Scoop manifest. |
| FreeBSD | [`dist/freebsd/`](https://github.com/zeroclaw-labs/zeroclaw/tree/master/dist/freebsd) | `rc.d` service scripts, a hardened variant, an end-to-end jail provisioner, and sample configs. See the [FreeBSD setup guide](./freebsd.md). |

Linux (systemd), macOS (launchd), and Windows (Task Scheduler) service units are generated for you by `zeroclaw service install` — see [Service management](./service.md). The files above cover the platforms that command does not target.
