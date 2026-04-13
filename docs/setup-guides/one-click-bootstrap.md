# One-Click Bootstrap

This page defines the fastest supported path to install and initialize ZeroClaw.

Last verified: **April 12, 2026**.

## Option 0: Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

## Option A (Recommended): Clone + local script

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

What it does:

1. Installs Rust via rustup if missing
2. Validates Rust version against project MSRV
3. `cargo install --path . --locked --force`
4. Runs `zeroclaw onboard` (interactive setup wizard)

## Option B: Remote one-liner

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

For high-security environments, prefer Option A so you can review the script before execution.

## Build profiles

```bash
./install.sh                                          # full (default features)
./install.sh --minimal                                # kernel only (~6.6MB)
./install.sh --minimal --features agent-runtime,channel-discord  # custom
```

`--minimal` builds the kernel: config, providers, memory, CLI chat. No agent runtime, no channels, no gateway. Ideal for SBCs and containers.

`--features` selects specific features. Works alone (adds to defaults) or with `--minimal` (builds from scratch).

To see all available features:

```bash
./install.sh --list-features
```

## Testing in isolation

Use `--prefix` to install everything into a scratch directory without touching your home:

```bash
./install.sh --prefix /tmp/zc-test --skip-onboard
/tmp/zc-test/.cargo/bin/zeroclaw --version

# Clean up
rm -rf /tmp/zc-test
```

Use `--dry-run` to preview what would happen without building:

```bash
./install.sh --dry-run --minimal --features agent-runtime,channel-discord
```

## Skip onboarding

```bash
./install.sh --skip-onboard
```

Configure later with `zeroclaw onboard`.

## Uninstall

```bash
./install.sh --uninstall
```

Removes the binary and optionally the config/data directory (`~/.zeroclaw/`).

## Pre-built binaries

For pre-built release binaries (no compilation required):

```bash
gh release download --repo zeroclaw-labs/zeroclaw --pattern "zeroclaw-$(uname -m)*"
```

Or download from [GitHub Releases](https://github.com/zeroclaw-labs/zeroclaw/releases/latest).

## Docker

See the `docker-compose.yml` at the repository root for containerized deployment.

## All flags

```bash
./install.sh --help
```

## Related docs

- [README.md](../../README.md)
- [commands-reference.md](../reference/cli/commands-reference.md)
- [providers-reference.md](../reference/api/providers-reference.md)
- [channels-reference.md](../reference/api/channels-reference.md)
