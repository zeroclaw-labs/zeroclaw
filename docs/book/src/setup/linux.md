# Linux

Install, update, run as a service, and uninstall, all Linux distributions.

## Install

```sh
./install.sh
```

That is the whole install. Run it from a clone, or pipe it from `curl`:

<div class="os-tabs-src">

#### sh

```sh
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

</div>

When the platform maps to a supported prebuilt target, an interactive run offers prebuilt or source installation; other platforms build from source. Source installs also offer app and optional-feature choices. For an unconfigured install, the installer then offers CLI or browser-based setup. The piped `curl | bash` path is noninteractive: it selects a prebuilt binary when available, falls back to a source build otherwise, skips the setup prompt, and prints [`zeroclaw quickstart`](../getting-started/quickstart.md) as the next step. Both paths install to Cargo's bin directory, usually `~/.cargo/bin/zeroclaw`. Pass `--help` for the full flag reference, or `--skip-quickstart` to install only.

### Homebrew (Linuxbrew)

<div class="os-tabs-src">

#### sh

```sh
brew install zeroclaw
```

</div>

Homebrew-on-Linux installs follow Homebrew's service path convention, your workspace lives under `$HOMEBREW_PREFIX/var/zeroclaw/` instead of `~/.zeroclaw/`. See [Service management](./service.md) for why this matters.

### NixOS

A multi-instance NixOS module is shipped in-tree. See [NixOS](./nixos.md).

### A note on `cargo binstall` and `nix run`

Neither works yet. `cargo binstall zeroclaw` resolves crate metadata from
crates.io, but ZeroClaw is not published there (`publish = false`), so there is
nothing for it to fetch; `nix run github:zeroclaw-labs/zeroclaw` does not launch
the agent because the flake exposes only a dev toolchain, not a runnable package
([#5987](https://github.com/zeroclaw-labs/zeroclaw/issues/5987)). `install.sh`
already does what `binstall` would (download a prebuilt release binary), so it
remains the supported one-liner.

## System dependencies

The core binary is statically linked where possible. Some features require system libraries:

| Feature | Package (Debian/Ubuntu) | Package (Arch) | Package (Fedora) |
|---|---|---|---|
| Docs translation (`cargo mdbook sync`) | `gettext` | `gettext` | `gettext` |
| Browser tool (playwright) | `libnss3`, `libatk1.0-0`, `libcups2` (see `playwright --help`) | `nss`, `atk`, `cups` | `nss`, `atk`, `cups` |
| Audio (TTS, voice channels) | `libasound2-dev` | `alsa-lib` | `alsa-lib-devel` |

The hardware feature (GPIO / I2C / SPI on a Pi) uses the pure-Rust `rppal` driver and needs no extra system library; it talks to `/dev/gpiomem`, `/dev/spidev*`, and `/dev/i2c-*` directly. What it does need is device access: enable the SPI/I2C interfaces and put the service user in the `gpio`, `spi`, and `i2c` groups (see [SBC / Raspberry Pi](#sbc--raspberry-pi) below).

Most deployments don't need any of these.

## Running as a service

Systemd is the default. OpenRC is detected and supported as a fallback.

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service install
zeroclaw service start
zeroclaw service status
```

</div>

Logs go to the systemd journal by default:

<div class="os-tabs-src">

#### sh

```sh
journalctl --user -u zeroclaw -f
```

</div>

Full details: [Service management](./service.md).

### SBC / Raspberry Pi

On a Raspberry Pi or similar SBC, build with the hardware feature:

<div class="os-tabs-src">

#### sh

```sh
./install.sh --source --features hardware
```

</div>

For hardware access without running as root, the service user needs the `gpio`, `spi`, and `i2c` groups. The user-level unit that `zeroclaw service install` writes does not set these; use the system-level Pi unit template at [`scripts/zeroclaw.service`](https://github.com/zeroclaw-labs/zeroclaw/blob/master/scripts/zeroclaw.service), which includes `SupplementaryGroups=gpio spi i2c`. Either way, verify your user is in those groups:

<div class="os-tabs-src">

#### sh

```sh
getent group gpio spi i2c
sudo usermod -aG gpio,spi,i2c $USER
# re-login for group changes to take effect
```

</div>

## Update

Re-run the installer, it detects the existing install and upgrades in place:

<div class="os-tabs-src">

#### sh

```sh
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash -s -- --skip-quickstart
```

</div>

Or from a clone:

<div class="os-tabs-src">

#### sh

```sh
cd /path/to/zeroclaw
git pull
./install.sh --skip-quickstart
```

</div>

If installed via Homebrew instead:

<div class="os-tabs-src">

#### sh

```sh
brew update && brew upgrade zeroclaw
```

</div>

After updating, restart the service:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service restart
```

</div>

## Uninstall

If you installed with the bootstrap script, use the same script to uninstall:

<div class="os-tabs-src">

#### sh

```sh
./install.sh --uninstall
```

</div>

Stop and remove the service:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service stop
zeroclaw service uninstall
```

</div>

Remove the binary:

<div class="os-tabs-src">

#### sh

```sh
# cargo install / bootstrap
rm ~/.cargo/bin/zeroclaw

# Homebrew
brew uninstall zeroclaw
```

</div>

Remove config and workspace (optional: this deletes conversation history):

<div class="os-tabs-src">

#### sh

```sh
rm -rf ~/.zeroclaw ~/.config/zeroclaw
```

</div>

## Next

- [Service management](./service.md): systemd unit details, logs, auto-start
- [Quickstart](../getting-started/quickstart.md): once installed, getting talking
- [Operations → Overview](../ops/overview.md): running in production
