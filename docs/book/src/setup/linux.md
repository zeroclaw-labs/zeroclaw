# Linux

Install, update, run as a service, and uninstall — all Linux distributions.

## Install

`install.sh` is the preferred path on every Linux distro. Pipe it from `curl`, or clone and run it locally — both do the same thing.

### Option 1 — `install.sh` via curl (fastest)

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

### Option 2 — `install.sh` from a clone

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

### What the installer does

1. Detects your distribution and architecture
2. Asks whether you want a prebuilt binary or to build from source (the default is interactive — non-interactive shells default to prebuilt when available)
3. Places the binary at `~/.cargo/bin/zeroclaw`
4. Runs `zeroclaw onboard` to complete first-time setup

Flags:

```bash
./install.sh --prebuilt                      # always prebuilt, skip the prompt
./install.sh --source                        # always build from source
./install.sh --minimal                       # kernel only (~6.6 MB)
./install.sh --source --features agent-runtime,channel-discord   # custom features
./install.sh --skip-onboard                  # install only; run `zeroclaw onboard` later
./install.sh --list-features                 # print available features and exit
./install.sh --help                          # full flag reference
```

### Option 3 — Homebrew (Linuxbrew)

```bash
brew install zeroclaw
zeroclaw onboard
```

Homebrew-on-Linux installs follow Homebrew's service path convention — your workspace lives under `$HOMEBREW_PREFIX/var/zeroclaw/` instead of `~/.zeroclaw/`. See [Service management](./service.md) for why this matters.

## System dependencies

The core binary is statically linked where possible. Some features require system libraries:

| Feature | Package (Debian/Ubuntu) | Package (Arch) | Package (Fedora) |
|---|---|---|---|
| Docs translation (`cargo mdbook sync`) | `gettext` | `gettext` | `gettext` |
| Hardware (GPIO / I2C / SPI) | `libgpiod-dev` | `libgpiod` | `libgpiod-devel` |
| Browser tool (playwright) | `libnss3`, `libatk1.0-0`, `libcups2` (see `playwright --help`) | `nss`, `atk`, `cups` | `nss`, `atk`, `cups` |
| Audio (TTS, voice channels) | `libasound2-dev` | `alsa-lib` | `alsa-lib-devel` |

Most deployments don't need any of these.

## Running as a service

Systemd is the default. OpenRC is detected and supported as a fallback.

```bash
zeroclaw service install
zeroclaw service start
zeroclaw service status
```

Logs go to the systemd journal by default:

```bash
journalctl --user -u zeroclaw -f
```

Full details: [Service management](./service.md).

### SBC / Raspberry Pi

On a Raspberry Pi or similar SBC, build with the hardware feature:

```bash
./install.sh --source --features hardware
```

The stock systemd unit includes `SupplementaryGroups=gpio spi i2c` so the service user can access hardware without running as root. Verify your user is in those groups:

```bash
getent group gpio spi i2c
sudo usermod -aG gpio,spi,i2c $USER
# re-login for group changes to take effect
```

## Update

Re-run the installer — it detects the existing install and upgrades in place:

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash -s -- --skip-onboard
```

Or from a clone:

```bash
cd /path/to/zeroclaw
git pull
./install.sh --skip-onboard
```

If installed via Homebrew instead:

```bash
brew update && brew upgrade zeroclaw
```

After updating, restart the service:

```bash
zeroclaw service restart
```

## Uninstall

Stop and remove the service:

```bash
zeroclaw service stop
zeroclaw service uninstall
```

Remove the binary:

```bash
# cargo install / bootstrap
rm ~/.cargo/bin/zeroclaw

# Homebrew
brew uninstall zeroclaw
```

Remove config and workspace (optional — this deletes conversation history):

```bash
rm -rf ~/.zeroclaw ~/.config/zeroclaw
```

## Next

- [Service management](./service.md) — systemd unit details, logs, auto-start
- [Quick start](../getting-started/quick-start.md) — once installed, getting talking
- [Operations → Overview](../ops/overview.md) — running in production
