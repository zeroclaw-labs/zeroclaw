# ZeroClaw Development Environment

A fully containerized development sandbox for ZeroClaw agents. This environment allows you to develop, test, and debug the agent in isolation without modifying your host system.

## Directory Structure

- **`agent/`**: (Merged into root Dockerfile)
    - The development image is built from the root `Dockerfile` using the `dev` stage (`target: dev`).
    - Based on `debian:bookworm-slim` (unlike production `distroless`).
    - Includes `bash`, `curl`, and debug tools.
- **`sandbox/`**: Dockerfile for the simulated user environment.
    - Based on `ubuntu:22.04`.
    - Pre-loaded with `git`, `python3`, `nodejs`, `npm`, `gcc`, `make`.
    - Simulates a real developer machine.
- **`docker-compose.yml`**: Defines the services and `dev-net` network.
- **`cli.sh`**: Helper script to manage the lifecycle.

## Usage

Run all commands from the repository root using the helper script:

### 1. Start Environment

```bash
./dev/cli.sh up
```

Builds the agent from source and starts both containers.

### 2. Enter Agent Container (`zeroclaw-dev`)

```bash
./dev/cli.sh agent
```

Use this to run `zeroclaw` CLI commands manually, debug the binary, or check logs internally.

- **Path**: `/zeroclaw-data`
- **User**: `nobody` (65534)

### 3. Enter Sandbox (`sandbox`)

```bash
./dev/cli.sh shell
```

Use this to act as the "user" or "environment" the agent interacts with.

- **Path**: `/home/developer/workspace`
- **User**: `developer` (sudo-enabled)

### 4. Development Cycle

1. Make changes to Rust code in `src/`.
2. Rebuild the agent:
    ```bash
    ./dev/cli.sh build
    ```
3. Test changes inside the container:
    ```bash
    ./dev/cli.sh agent
    # inside container:
    zeroclaw --version
    ```

### 5. Persistence & Shared Workspace

The local `playground/` directory (in repo root) is mounted as the shared workspace:

- **Agent**: `/zeroclaw-data/workspace`
- **Sandbox**: `/home/developer/workspace`

Files created by the agent are visible to the sandbox user, and vice versa.

The agent configuration lives in `target/.zeroclaw` (mounted to `/zeroclaw-data/.zeroclaw`), so settings persist across container rebuilds.

### 6. Cleanup

Stop containers and remove volumes and generated config:

```bash
./dev/cli.sh clean
```

**Note:** This removes `target/.zeroclaw` (config/DB) but leaves the `playground/` directory intact. To fully wipe everything, manually delete `playground/`.

## Local CI/CD (Docker-Only)

Use this when you want CI-style validation without relying on GitHub Actions and without running Rust toolchain commands on your host.

### 1. Build the local CI image

```bash
./dev/ci.sh build-image
```

### 2. Run full local CI pipeline

```bash
./dev/ci.sh all
```

This runs inside a container:

- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets -- -D clippy::correctness`
- `cargo test --locked --verbose`
- `cargo build --release --locked --verbose`
- `cargo deny check licenses sources`
- `cargo audit`
- Docker smoke build (`docker build --target dev ...` + `--version` check)

### 3. Run targeted stages

```bash
./dev/ci.sh lint
./dev/ci.sh test
./dev/ci.sh build
./dev/ci.sh deny
./dev/ci.sh audit
./dev/ci.sh security
./dev/ci.sh docker-smoke
```

Note: local `deny` focuses on license/source policy; advisory scanning is handled by `audit`.

### 4. Enter CI container shell

```bash
./dev/ci.sh shell
```

### 5. Optional shortcut via existing dev CLI

```bash
./dev/cli.sh ci
./dev/cli.sh ci lint
```

### Isolation model

- Rust compilation, tests, and audit/deny tools run in `zeroclaw-local-ci` container.
- Your host filesystem is mounted at `/workspace`; no host Rust toolchain is required.
- Cargo build artifacts are written to container volume `/ci-target` (not your host `target/`).
- Docker smoke stage uses your Docker daemon to build image layers, but build steps execute in containers.

### Build cache notes

- Both `Dockerfile` and `dev/ci/Dockerfile` use BuildKit cache mounts for Cargo registry/git data.
- Local CI reuses named Docker volumes for Cargo registry/git and target outputs.
- The CI image keeps Rust toolchain defaults from `rust:1.92-slim` (no custom `CARGO_HOME`/`RUSTUP_HOME` overrides), preventing repeated toolchain bootstrapping on each run.
