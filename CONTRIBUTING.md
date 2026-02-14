# Contributing to ZeroClaw

Thanks for your interest in contributing to ZeroClaw! This guide will help you get started.

## Development Setup

```bash
# Clone the repo
git clone https://github.com/theonlyhennygod/zeroclaw.git
cd zeroclaw

# Enable the pre-push hook (runs fmt, clippy, tests before every push)
git config core.hooksPath .githooks

# Build
cargo build

# Run tests (all must pass)
cargo test

# Format & lint (must pass before PR)
cargo fmt && cargo clippy -- -D warnings

# Release build (~3.4MB)
cargo build --release
```

### Pre-push hook

The repo includes a pre-push hook in `.githooks/` that enforces `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` before every push. Enable it with `git config core.hooksPath .githooks`.

To skip it during rapid iteration:

```bash
git push --no-verify
```

> **Note:** CI runs the same checks, so skipped hooks will be caught on the PR.

## Architecture: Trait-Based Pluggability

ZeroClaw's architecture is built on **traits** — every subsystem is swappable. This means contributing a new integration is as simple as implementing a trait and registering it in the factory function.

```
src/
├── providers/       # LLM backends     → Provider trait
├── channels/        # Messaging         → Channel trait
├── observability/   # Metrics/logging   → Observer trait
├── runtime/         # Platform adapters → RuntimeAdapter trait
├── tools/           # Agent tools       → Tool trait
├── memory/          # Persistence/brain → Memory trait
└── security/        # Sandboxing        → SecurityPolicy
```

## How to Add a New Provider

Create `src/providers/your_provider.rs`:

```rust
use async_trait::async_trait;
use anyhow::Result;
use crate::providers::traits::Provider;

pub struct YourProvider {
    api_key: String,
    client: reqwest::Client,
}

impl YourProvider {
    pub fn new(api_key: Option<&str>) -> Self {
        Self {
            api_key: api_key.unwrap_or_default().to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Provider for YourProvider {
    async fn chat(&self, message: &str, model: &str, temperature: f64) -> Result<String> {
        // Your API call here
        todo!()
    }
}
```

Then register it in `src/providers/mod.rs`:

```rust
"your_provider" => Ok(Box::new(your_provider::YourProvider::new(api_key))),
```

## How to Add a New Channel

Create `src/channels/your_channel.rs`:

```rust
use async_trait::async_trait;
use anyhow::Result;
use tokio::sync::mpsc;
use crate::channels::traits::{Channel, ChannelMessage};

pub struct YourChannel { /* config fields */ }

#[async_trait]
impl Channel for YourChannel {
    fn name(&self) -> &str { "your_channel" }

    async fn send(&self, message: &str, recipient: &str) -> Result<()> {
        // Send message via your platform
        todo!()
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Listen for incoming messages, forward to tx
        todo!()
    }

    async fn health_check(&self) -> bool { true }
}
```

## How to Add a New Observer

Create `src/observability/your_observer.rs`:

```rust
use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};

pub struct YourObserver { /* client, config, etc. */ }

impl Observer for YourObserver {
    fn record_event(&self, event: &ObserverEvent) {
        // Push event to your backend
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        // Push metric to your backend
    }

    fn name(&self) -> &str { "your_observer" }
}
```

## How to Add a New Tool

Create `src/tools/your_tool.rs`:

```rust
use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};
use crate::tools::traits::{Tool, ToolResult};

pub struct YourTool { /* security policy, config, etc. */ }

#[async_trait]
impl Tool for YourTool {
    fn name(&self) -> &str { "your_tool" }

    fn description(&self) -> &str { "Does something useful" }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input": { "type": "string", "description": "The input" }
            },
            "required": ["input"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let input = args["input"].as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'input'"))?;
        Ok(ToolResult {
            success: true,
            output: format!("Processed: {input}"),
            error: None,
        })
    }
}
```

## Pull Request Checklist

- [ ] `cargo fmt` — code is formatted
- [ ] `cargo clippy -- -D warnings` — no warnings
- [ ] `cargo test` — all 129+ tests pass
- [ ] New code has inline `#[cfg(test)]` tests
- [ ] No new dependencies unless absolutely necessary (we optimize for binary size)
- [ ] README updated if adding user-facing features
- [ ] Follows existing code patterns and conventions

## Commit Convention

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add Anthropic provider
fix: path traversal edge case with symlinks
docs: update contributing guide
test: add heartbeat unicode parsing tests
refactor: extract common security checks
chore: bump tokio to 1.43
```

## Code Style

- **Minimal dependencies** — every crate adds to binary size
- **Inline tests** — `#[cfg(test)] mod tests {}` at the bottom of each file
- **Trait-first** — define the trait, then implement
- **Security by default** — sandbox everything, allowlist, never blocklist
- **No unwrap in production code** — use `?`, `anyhow`, or `thiserror`

## Reporting Issues

- **Bugs**: Include OS, Rust version, steps to reproduce, expected vs actual
- **Features**: Describe the use case, propose which trait to extend
- **Security**: See [SECURITY.md](SECURITY.md) for responsible disclosure

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
