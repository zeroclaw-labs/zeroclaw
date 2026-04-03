# ZeroClaw Troubleshooting

This guide focuses on common setup/runtime failures and fast resolution paths.

Last verified: **February 20, 2026**.

## Installation / Bootstrap

### `cargo` not found

Symptom:

- bootstrap exits with `cargo is not installed`

Fix:

```bash
./install.sh --install-rust
```

Or install from <https://rustup.rs/>.

### Missing system build dependencies

Symptom:

- build fails due to compiler or `pkg-config` issues

Fix:

```bash
./install.sh --install-system-deps
```

### Build fails on low-RAM / low-disk hosts

Symptoms:

- `cargo build --release` is killed (`signal: 9`, OOM killer, or `cannot allocate memory`)
- Build crashes after adding swap because disk space runs out

Why this happens:

- Runtime memory (<5MB for common operations) is not the same as compile-time memory.
- Full source build can require **2 GB RAM + swap** and **6+ GB free disk**.
- Enabling swap on a tiny disk can avoid RAM OOM but still fail due to disk exhaustion.

Preferred path for constrained machines:

```bash
./install.sh --prefer-prebuilt
```

Binary-only mode (no source fallback):

```bash
./install.sh --prebuilt-only
```

If you must compile from source on constrained hosts:

1. Add swap only if you also have enough free disk for both swap + build output.
1. Limit cargo parallelism:

```bash
CARGO_BUILD_JOBS=1 cargo build --release --locked
```

1. Reduce heavy features when Matrix is not required:

```bash
cargo build --release --locked --features hardware
```

1. Cross-compile on a stronger machine and copy the binary to the target host.

### Build is very slow or appears stuck

Symptoms:

- `cargo check` / `cargo build` appears stuck at `Checking zeroclaw` for a long time
- repeated `Blocking waiting for file lock on package cache` or `build directory`

Why this happens in ZeroClaw:

- Matrix E2EE stack (`matrix-sdk`, `ruma`, `vodozemac`) is large and expensive to type-check.
- TLS + crypto native build scripts (`aws-lc-sys`, `ring`) add noticeable compile time.
- `rusqlite` with bundled SQLite compiles C code locally.
- Running multiple cargo jobs/worktrees in parallel causes lock contention.

Fast checks:

```bash
cargo check --timings
cargo tree -d
```

The timing report is written to `target/cargo-timings/cargo-timing.html`.

Faster local iteration (when Matrix channel is not needed):

```bash
cargo check
```

This uses the lean default feature set and can significantly reduce compile time.

To build with Matrix support explicitly enabled:

```bash
cargo check --features channel-matrix
```

To build with Matrix + Lark + hardware support:

```bash
cargo check --features hardware,channel-matrix,channel-lark
```

Lock-contention mitigation:

```bash
pgrep -af "cargo (check|build|test)|cargo check|cargo build|cargo test"
```

Stop unrelated cargo jobs before running your own build.

### `zeroclaw` command not found after install

Symptom:

- install succeeds but shell cannot find `zeroclaw`

Fix:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
which zeroclaw
```

Persist in your shell profile if needed.

## Runtime / Gateway

### OpenAI Codex subscription auth warns about config or streaming

Symptoms:

- `default_provider = "openai-codex"` is set, but runs still feel misconfigured
- config loading warns about unknown top-level `api_key` / `api_url`
- agent logs `provider streaming failed, falling back to non-streaming chat`

Checks:

```bash
zeroclaw auth status
zeroclaw auth login --provider openai-codex --device-code
zeroclaw agent --provider openai-codex -m "hello"
```

Use this minimal `config.toml` shape for normal subscription auth:

```toml
default_provider = "openai-codex"
default_model = "gpt-5-codex"
```

Notes:

- Standard OpenAI Codex subscription auth uses stored auth profiles,
  so top-level `api_key` / `api_url` should usually stay unset.
- `api_key` / `api_url` are only needed for custom OpenAI-compatible
  gateways or other explicit endpoint overrides.
- The streaming fallback warning by itself is not an auth failure;
  ZeroClaw retries the request in non-streaming mode.

### Gateway unreachable

Checks:

```bash
zeroclaw status
zeroclaw doctor
```

Verify `~/.zeroclaw/config.toml`:

- `[gateway].host` (default `127.0.0.1`)
- `[gateway].port` (default `42617`)
- `allow_public_bind` only when intentionally exposing LAN/public interfaces

### Pairing / auth failures on webhook

Checks:

1. Ensure pairing completed (`/pair` flow)
2. Ensure bearer token is current
3. Re-run diagnostics:

```bash
zeroclaw doctor
```

## Channel Issues

### Telegram conflict: `terminated by other getUpdates request`

Cause:

- multiple pollers using same bot token

Fix:

- keep only one active runtime for that token
- stop extra `zeroclaw daemon` / `zeroclaw channel start` processes

### Channel unhealthy in `channel doctor`

Checks:

```bash
zeroclaw channel doctor
```

Then verify channel-specific credentials + allowlist fields in config.

## Service Mode

### Service installed but not running

Checks:

```bash
zeroclaw service status
```

There is no `zeroclaw service logs` command. If the service is
installed, inspect platform logs instead:

- Linux: `journalctl --user -u zeroclaw.service -f`
- macOS: `log stream --predicate 'process == "zeroclaw"'`
- If you are running `zeroclaw daemon` directly in a terminal, use
  that foreground output instead of service log commands.

Recovery:

```bash
zeroclaw service stop
zeroclaw service start
```

Linux logs:

```bash
journalctl --user -u zeroclaw.service -f
```

## Installer URL

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash
```

## Still Stuck?

Collect and include these outputs when filing an issue:

```bash
zeroclaw --version
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

Also include OS, install method, and sanitized config snippets (no secrets).

## Related Docs

- [operations-runbook.md](operations-runbook.md)
- [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md)
- [channels-reference.md](../reference/api/channels-reference.md)
- [network-deployment.md](network-deployment.md)
