# Contributing to Hrafn

Thank you for considering contributing to Hrafn. This document explains how we work together and what you can expect from us.

---

## Our Promises to Contributors

### 1. Every PR gets a response within 48 hours.

Accept, request changes, or explain why not. No exceptions. If a maintainer can't review in time, they comment to say when they will.

### 2. No silent closes.

If a PR is closed without merge, there is a written explanation. "Not aligned with the roadmap" is not enough -- we explain *why* and suggest alternatives (e.g. "consider implementing this as an MCP plugin instead").

### 3. Your code stays your code.

- Maintainers never re-submit a contributor's work under their own name.
- If a PR needs to be reverted and re-applied, the original author is asked to submit the fix. If they are unavailable after 7 days, a maintainer may re-apply it with the original author preserved in `git author` and a link to the original PR.
- `Co-authored-by` is for actual co-authorship, not for crediting someone whose code you copied.
- Git author history is immutable. Batch reverts that include functional community code require a public explanation in the revert PR.

### 4. RFCs before big features.

Features that touch more than one module (channel + gateway, tool + config, etc.) require a discussion or RFC before implementation. Open a GitHub Discussion with the `RFC` label. This prevents wasted effort on both sides.

### 5. Transparent roadmap.

The GitHub Projects board is the source of truth for what's planned, in progress, and done. It is updated weekly.

---

## How to Contribute

### Reporting bugs

Open an issue. Include:
- Hrafn version (`hrafn --version`)
- OS and architecture
- Steps to reproduce
- Expected vs. actual behavior
- Logs (redact secrets and personal data)

### Suggesting features

Open a GitHub Discussion with the `Feature Request` label. Describe the problem you're solving, not just the solution you want.

### Submitting code

1. **Check the roadmap.** Is someone already working on this? Is there an RFC?
2. **Open an issue first** for non-trivial changes. A 5-minute conversation can save hours of wasted work.
3. **Fork and branch.** Branch from `main`. Use descriptive branch names.
4. **Write tests.** New features need tests. Bug fixes need a regression test.
5. **Follow the style.** Run `cargo fmt` and `cargo clippy -D warnings` before pushing.
6. **Keep PRs focused.** One feature or fix per PR. If you find an unrelated bug while working, open a separate issue.
7. **Write a clear PR description.** What changed, why, and how to test it.

### Contribution ladder

| Level | Activity |
|-------|----------|
| User | Use Hrafn, report issues, give feedback |
| Tester | Test OC Bridge plugins, validate demand |
| Contributor | Submit PRs (bug fixes, docs, features) |
| Porter | Take a plugin from the port queue, implement in Rust |
| Reviewer | Review PRs, enforce quality standards |
| Maintainer | Trait design, core architecture, release management |

Progression is based on trust built through consistent, quality contributions. There is no application process -- maintainers will invite contributors to the next level when the time is right.

---

## Development Setup

```bash
git clone https://github.com/5queezer/hrafn.git
cd hrafn
cargo build
cargo test --locked
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

For a release build: `cargo build --release --locked`

To build with specific features only:

```bash
cargo build --no-default-features --features "channel-telegram,tool-a2a,memory-muninndb"
```

---

## Code Style

- **Minimal dependencies.** Every crate adds to binary size. Justify new deps in your PR.
- **Trait-first.** Define the interface, then implement. Traits are Hrafn's plugin API.
- **Inline tests.** `#[cfg(test)] mod tests {}` at the bottom of each file.
- **No `unwrap()` in production code.** Use `?`, `anyhow`, or `thiserror`.
- **Security by default.** Sandbox, allowlist, never blocklist.

---

## How to Add an Integration

Hrafn's architecture is trait-based. Every subsystem (provider, channel, tool, memory) is a trait implementation registered in a factory. Adding a new integration means implementing a trait and adding one line to `mod.rs`.

```
src/
├── providers/    # LLM backends      → Provider trait
├── channels/     # Messaging          → Channel trait
├── tools/        # Agent tools        → Tool trait
├── memory/       # Persistence        → Memory trait
└── gateway/      # HTTP/WS control    → (core, not pluggable)
```

### New Provider

Create `src/providers/your_provider.rs`:

```rust
use async_trait::async_trait;
use anyhow::Result;
use crate::providers::traits::Provider;

pub struct YourProvider {
    api_key: String,
    client: reqwest::Client,
}

#[async_trait]
impl Provider for YourProvider {
    async fn chat(&self, message: &str, model: &str, temperature: f64) -> Result<String> {
        todo!()
    }
}
```

Register in `src/providers/mod.rs`:

```rust
"your_provider" => Ok(Box::new(your_provider::YourProvider::new(api_key))),
```

### New Channel

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
    async fn send(&self, message: &str, recipient: &str) -> Result<()> { todo!() }
    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> { todo!() }
    async fn health_check(&self) -> bool { true }
}
```

### New Tool

Create `src/tools/your_tool.rs`:

```rust
use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};
use crate::tools::traits::{Tool, ToolResult};

pub struct YourTool { /* config */ }

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
        Ok(ToolResult { success: true, output: format!("Processed: {input}"), error: None })
    }
}
```

Wrap each new integration in a feature gate:

```rust
#[cfg(feature = "tool-your-tool")]
pub mod your_tool;
```

---

## Code Standards

### Must pass before merge

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- All existing tests pass
- New code has tests (unit and/or integration)

### Security-relevant code

Changes to authentication, cryptography, network-facing endpoints, or SSRF protection require review from at least one other maintainer or trusted contributor. No self-merging on security code.

### AI-assisted code

AI-generated code is welcome, but:
- The submitter must understand and be able to explain every line.
- "Claude wrote it" is not a valid response to a review question.
- AI co-authorship should be noted in the commit message (`Co-Authored-By:`).

---

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(a2a): add outbound task delegation
fix(gateway): respect path_prefix in pairing endpoint
docs(config): add [a2a] section to config reference
refactor(memory): extract trait for storage backends
test(tools): add SSRF validation edge cases
chore(ci): add feature combination matrix
```

Scope should match the module: `core`, `gateway`, `channel-telegram`, `tool-a2a`, `memory-muninndb`, `config`, `docs`, `ci`.

---

## PR Labels

| Label | Meaning |
|-------|---------|
| `PR: Feature` | New functionality |
| `PR: Fix` | Bug fix |
| `PR: Refactor` | Code improvement, no behavior change |
| `PR: Docs` | Documentation only |
| `PR: Port` | Native Rust port of an OC Bridge plugin |
| `PR: Security` | Security-relevant change (triggers mandatory second review) |
| `RFC` | Request for Comments (Discussion, not PR) |

---

## OC Bridge Plugins

The OC Bridge lets Hrafn users test OpenClaw plugins via MCP without a native Rust implementation. The bridge is transitional -- plugins that see sustained community usage get queued for native porting.

### Testing an OC Bridge plugin

See `docs/oc-bridge.md` for setup instructions.

### Porting a plugin to native Rust

1. Check the port queue (GitHub Project board, "Port Queue" column).
2. Comment on the issue to claim it.
3. Implement against the relevant Hrafn trait (`Tool`, `Channel`, `Provider`, `Memory`).
4. Include tests that match or exceed the bridge plugin's coverage.
5. Submit with label `PR: Port`.

---

## Community Calls

We hold weekly community calls (day/time TBD). Agenda is posted in GitHub Discussions 24 hours before. Anyone can add a topic. Calls are recorded and summarized in a Discussion post.

---

## Code of Conduct

Be respectful. Give constructive feedback. Assume good intent. We are here to build something good together, not to compete with each other.

If you experience or witness unacceptable behavior, contact the maintainer directly.

---

## License

By contributing, you agree that your contributions are licensed under the same license as the project (Apache-2.0). You retain copyright of your work.

---

## Origin

Hrafn originated as a fork of [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw). We thank the ZeroClaw contributors for the foundation. This project exists because we believe open-source communities deserve transparent governance and respect for contributors' work.
