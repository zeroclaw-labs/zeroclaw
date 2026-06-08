# Linux

Install, update, run as a service, and uninstall, all Linux distributions.

## Install

`install.sh` is the preferred path on every Linux distro. Pipe it from `curl`, or clone and run it locally, both do the same thing.

### Option 1: `install.sh` via curl (fastest)

<div class="os-tabs-src">

#### sh

```sh
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

</div>

### Option 2: `install.sh` from a clone

<div class="os-tabs-src">

#### sh

```sh
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

</div>

### What the installer does

1. Detects your distribution and architecture
2. Asks whether you want a prebuilt binary or to build from source (the default is interactive, non-interactive shells default to prebuilt when available)
3. Places the binary at `~/.cargo/bin/zeroclaw`
4. Runs `zeroclaw quickstart` to complete first-time setup

Flags:

<div class="os-tabs-src">

#### sh

```sh
./install.sh --prebuilt                      # always prebuilt, skip the prompt
./install.sh --source                        # always build from source
./install.sh --minimal                       # foundation only, no default features
./install.sh --source --features agent-runtime,channel-discord   # custom features
./install.sh --skip-quickstart                  # install only; run `zeroclaw quickstart` later
./install.sh --list-features                 # print available features and exit
./install.sh --help                          # full flag reference
```

</div>

### Option 3: Homebrew (Linuxbrew)

<div class="os-tabs-src">

#### sh

```sh
brew install zeroclaw
zeroclaw quickstart
```

</div>

Homebrew-on-Linux installs follow Homebrew's service path convention, your workspace lives under `$HOMEBREW_PREFIX/var/zeroclaw/` instead of `~/.zeroclaw/`. See [Service management](./service.md) for why this matters.

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

The stock systemd unit includes `SupplementaryGroups=gpio spi i2c` so the service user can access hardware without running as root. Verify your user is in those groups:

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
