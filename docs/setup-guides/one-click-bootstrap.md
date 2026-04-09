# One-Click Bootstrap

This page defines the fastest supported path to install and initialize ZeroClaw.

Last verified: **April 2026**.

## Package Managers (Recommended)

### Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

### Cargo binstall (Cross-platform)

The fastest way to install ZeroClaw from crates.io without compiling:

```bash
# Install cargo-binstall if you don't have it
cargo install cargo-binstall

# Install ZeroClaw
cargo binstall zeroclaw
```

### Nix (Linux/macOS)

Run directly without installing:

```bash
nix run github:zeroclaw-labs/zeroclaw
```

Or install via nix profile:

```bash
nix profile install github:zeroclaw-labs/zeroclaw
```

### Scoop (Windows)

```bash
scoop install zeroclaw
```

## Pre-built Binaries

### Docker

Run ZeroClaw without any local installation:

```bash
docker run -d --name zeroclaw \
  -p 42617:42617 \
  -v "$HOME/.zeroclaw:/zeroclaw-data/.zeroclaw" \
  -v "$HOME/zeroclaw-workspace:/zeroclaw-data/workspace" \
  ghcr.io/zeroclaw-labs/zeroclaw:latest
```

Or use docker-compose for persistent deployment:

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
docker compose up -d
```

### GitHub Releases

Download pre-built binaries for your platform from [GitHub Releases](https://github.com/zeroclaw-labs/zeroclaw/releases/latest):

- Linux: `x86_64`, `aarch64`, `armv7`
- macOS: `x86_64`, `aarch64` (Apple Silicon)
- Windows: `x86_64`

Extract and place the binary in your `PATH`.

## Build from Source

### Cargo install (requires Rust)

If you have Rust installed:

```bash
cargo install zeroclaw
```

### Clone + local script (development)

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./install.sh
```

What it does by default:

1. `cargo build --release --locked`
2. `cargo install --path . --force --locked`

### Resource preflight and pre-built flow

Source builds typically require at least:

- **2 GB RAM + swap**
- **6 GB free disk**

When resources are constrained, bootstrap now attempts a pre-built binary first.

```bash
./install.sh --prefer-prebuilt
```

To require binary-only installation and fail if no compatible release asset exists:

```bash
./install.sh --prebuilt-only
```

To bypass pre-built flow and force source compilation:

```bash
./install.sh --force-source-build
```

## Dual-mode bootstrap

Default behavior is **app-only** (build/install ZeroClaw) and expects existing Rust toolchain.

For fresh machines, enable environment bootstrap explicitly:

```bash
./install.sh --install-system-deps --install-rust
```

Notes:

- `--install-system-deps` installs compiler/build prerequisites (may require `sudo`).
- `--install-rust` installs Rust via `rustup` when missing.
- `--prefer-prebuilt` tries release binary download first, then falls back to source build.
- `--prebuilt-only` disables source fallback.
- `--force-source-build` disables pre-built flow entirely.

## Option B: Remote one-liner

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

For high-security environments, prefer Option A so you can review the script before execution.

If you run Option B outside a repository checkout, the install script automatically clones a temporary workspace, builds, installs, and then cleans it up.

## Optional onboarding modes

### Containerized onboarding (Docker)

```bash
./install.sh --docker
```

This builds a local ZeroClaw image and launches onboarding inside a container while
persisting config/workspace to `./.zeroclaw-docker`.

Container CLI defaults to `docker`. If Docker CLI is unavailable and `podman` exists,
the installer auto-falls back to `podman`. You can also set `ZEROCLAW_CONTAINER_CLI`
explicitly (for example: `ZEROCLAW_CONTAINER_CLI=podman ./install.sh --docker`).

For Podman, the installer runs with `--userns keep-id` and `:Z` volume labels so
workspace/config mounts remain writable inside the container.

If you add `--skip-build`, the installer skips local image build. It first tries the local
Docker tag (`ZEROCLAW_DOCKER_IMAGE`, default: `zeroclaw-bootstrap:local`); if missing,
it pulls `ghcr.io/zeroclaw-labs/zeroclaw:latest` and tags it locally before running.

### Stopping and restarting a Docker/Podman container

After `./install.sh --docker` finishes, the container exits. Your config and workspace
are persisted in the data directory (default: `./.zeroclaw-docker`, or `~/.zeroclaw-docker`
when bootstrapping via `curl | bash`). You can override this path with `ZEROCLAW_DOCKER_DATA_DIR`.

**Do not re-run `install.sh`** to restart -- it will rebuild the image and re-run onboarding.
Instead, start a new container from the existing image and mount the persisted data directory.

#### Using the repository docker-compose.yml

The simplest way to run ZeroClaw long-term in Docker/Podman is with the provided
`docker-compose.yml` at the repository root. It uses a named volume (`zeroclaw-data`)
and sets `restart: unless-stopped` so the container survives reboots.

```bash
# Start (detached)
docker compose up -d

# Stop
docker compose down

# Restart after stopping
docker compose up -d
```

Replace `docker` with `podman` if you use Podman.

#### Manual container run (using install.sh data directory)

If you installed via `./install.sh --docker` and want to reuse the `.zeroclaw-docker`
data directory without compose:

```bash
# Docker
docker run -d --name zeroclaw \
  --restart unless-stopped \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace" \
  -e HOME=/zeroclaw-data \
  -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway

# Podman (add --userns keep-id and :Z volume labels)
podman run -d --name zeroclaw \
  --restart unless-stopped \
  --userns keep-id \
  --user "$(id -u):$(id -g)" \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw:Z" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace:Z" \
  -e HOME=/zeroclaw-data \
  -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway
```

#### Common lifecycle commands

```bash
# Stop the container (preserves data)
docker stop zeroclaw

# Start a stopped container (config and workspace are intact)
docker start zeroclaw

# View logs
docker logs -f zeroclaw

# Remove the container (data in volumes/.zeroclaw-docker is preserved)
docker rm zeroclaw

# Check health
docker exec zeroclaw zeroclaw status
```

#### Environment variables

When running manually, pass provider configuration as environment variables
or ensure they are already saved in the persisted `config.toml`:

```bash
docker run -d --name zeroclaw \
  -e API_KEY="sk-..." \
  -e PROVIDER="openrouter" \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace" \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway
```

If you already ran `onboard` during the initial install, your API key and provider are
saved in `.zeroclaw-docker/.zeroclaw/config.toml` and do not need to be passed again.

### Quick onboarding (non-interactive)

```bash
./install.sh --api-key "sk-..." --provider openrouter
```

Or with environment variables:

```bash
ZEROCLAW_API_KEY="sk-..." ZEROCLAW_PROVIDER="openrouter" ./install.sh
```

## Useful flags

- `--install-system-deps`
- `--install-rust`
- `--skip-build` (in `--docker` mode: use local image if present, otherwise pull `ghcr.io/zeroclaw-labs/zeroclaw:latest`)
- `--skip-install`
- `--provider <id>`

See all options:

```bash
./install.sh --help
```

## Related docs

- [README.md](../README.md)
- [commands-reference.md](../reference/cli/commands-reference.md)
- [providers-reference.md](../reference/api/providers-reference.md)
- [channels-reference.md](../reference/api/channels-reference.md)
