# ZeroClaw Troubleshooting

This guide focuses on common setup/runtime failures and fast resolution paths.

Last verified: **February 20, 2026**.

## Installation / Bootstrap

### `cargo` not found

Symptom:

- bootstrap exits with `cargo is not installed`

Fix:

```bash
./bootstrap.sh --install-rust
```

Or install from <https://rustup.rs/>.

### Missing system build dependencies

Symptom:

- build fails due to compiler or `pkg-config` issues

Fix:

```bash
./bootstrap.sh --install-system-deps
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
./bootstrap.sh --prefer-prebuilt
```

Binary-only mode (no source fallback):

```bash
./bootstrap.sh --prebuilt-only
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

## Web Access Issues

### `curl`/`wget` blocked in shell tool

Symptom:

- tool output includes `Command blocked: high-risk command is disallowed by policy`
- model says `curl`/`wget` is blocked

Why this happens:

- `curl`/`wget` are high-risk shell commands and may be blocked by autonomy policy.

Preferred fix:

- use purpose-built tools instead of shell fetch:
  - `http_request` for direct API/HTTP calls
  - `web_fetch` for page content extraction/summarization

Minimal config:

```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
provider = "fast_html2md"
allowed_domains = ["*"]
```

### `web_search_tool` fails with `403`/`429`

Symptom:

- tool output includes `DuckDuckGo search failed with status: 403` (or `429`)

Why this happens:

- some networks/proxies/rate limits block DuckDuckGo HTML search endpoint traffic.

Fix options:

1. Switch provider to Brave (recommended when you have an API key):

```toml
[web_search]
enabled = true
provider = "brave"
brave_api_key = "<SECRET>"
```

2. Switch provider to Firecrawl (if enabled in your build):

```toml
[web_search]
enabled = true
provider = "firecrawl"
api_key = "<SECRET>"
```

3. Keep DuckDuckGo for search, but use `web_fetch` to read pages once you have URLs.

### `web_fetch`/`http_request` says host is not allowed

Symptom:

- errors like `Host '<domain>' is not in http_request.allowed_domains`
- or `web_fetch tool is enabled but no allowed_domains are configured`

Fix:

- include exact domains or `"*"` for public internet access:

```toml
[http_request]
enabled = true
allowed_domains = ["*"]

[web_fetch]
enabled = true
allowed_domains = ["*"]
blocked_domains = []
```

Security notes:

- local/private network targets are blocked even with `"*"`
- keep explicit domain allowlists in production environments when possible

## Service Mode

### Service installed but not running

Checks:

```bash
zeroclaw service status
```

Recovery:

```bash
zeroclaw service stop
zeroclaw service start
```

Linux logs:

```bash
journalctl --user -u zeroclaw.service -f
```

## Legacy Installer Compatibility

Both still work:

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/bootstrap.sh | bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/install.sh | bash
```

`install.sh` is a compatibility entry and forwards/falls back to bootstrap behavior.

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
- [one-click-bootstrap.md](one-click-bootstrap.md)
- [channels-reference.md](channels-reference.md)
- [network-deployment.md](network-deployment.md)
