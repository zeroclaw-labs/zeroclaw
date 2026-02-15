# ZeroClaw Development Environment

A fully containerized development sandbox for ZeroClaw agents. This environment allows you to develop, test, and debug the agent in isolation without modifying your host system.

## Directory Structure

- **`agent/`**: Dockerfile for the Agent in development mode.
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
Stop containers and remove volumes (wipes workspace data):
```bash
./dev/cli.sh clean
```
