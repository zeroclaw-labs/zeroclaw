# Contributing to ZeroClaw

Thanks for your interest in contributing to ZeroClaw! This guide will help you get started.

## Development Setup

```bash
# Clone the repo
git clone https://github.com/zeroclaw-labs/zeroclaw.git
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

## Collaboration Tracks (Risk-Based)

To keep review throughput high without lowering quality, every PR should map to one track:

| Track | Typical scope | Required review depth |
|---|---|---|
| **Track A (Low risk)** | docs/tests/chore, isolated refactors, no security/runtime/CI impact | 1 maintainer review + green `CI Required Gate` |
| **Track B (Medium risk)** | providers/channels/memory/tools behavior changes | 1 subsystem-aware review + explicit validation evidence |
| **Track C (High risk)** | `src/security/**`, `src/runtime/**`, `src/gateway/**`, `.github/workflows/**`, access-control boundaries | 2-pass review (fast triage + deep risk review), rollback plan required |

When in doubt, choose the higher track.

## Documentation Optimization Principles

To keep docs useful under high PR volume, we use these rules:

- **Single source of truth**: policy lives in docs, not scattered across PR comments.
- **Decision-oriented content**: every checklist item should directly help accept/reject a change.
- **Risk-proportionate detail**: high-risk paths need deeper evidence; low-risk paths stay lightweight.
- **Side-effect visibility**: document blast radius, failure modes, and rollback before merge.
- **Automation assists, humans decide**: bots triage and label, but merge accountability stays human.

### Documentation System Map

| Doc | Primary purpose | When to update |
|---|---|---|
| `CONTRIBUTING.md` | contributor contract and readiness baseline | contributor expectations or policy changes |
| `docs/pr-workflow.md` | governance logic and merge contract | workflow/risk/merge gate changes |
| `docs/reviewer-playbook.md` | reviewer operating checklist | review depth or triage behavior changes |
| `docs/ci-map.md` | CI ownership and triage entry points | workflow trigger/job ownership changes |

## PR Definition of Ready (DoR)

Before requesting review, ensure all of the following are true:

- Scope is focused to a single concern.
- `.github/pull_request_template.md` is fully completed.
- Relevant local validation has been run (`fmt`, `clippy`, `test`, scenario checks).
- Security impact and rollback path are explicitly described.
- No personal/sensitive data is introduced in code/docs/tests/fixtures/logs/examples/commit messages.
- Tests/fixtures/examples use neutral project-scoped wording (no identity-specific or first-person phrasing).
- If identity-like wording is required, use ZeroClaw-centric labels only (for example: `ZeroClawAgent`, `ZeroClawOperator`, `zeroclaw_user`).
- Linked issue (or rationale for no issue) is included.

## PR Definition of Done (DoD)

A PR is merge-ready when:

- `CI Required Gate` is green.
- Required reviewers approved (including CODEOWNERS paths).
- Risk level matches changed paths (`risk: low/medium/high`).
- User-visible behavior, migration, and rollback notes are complete.
- Follow-up TODOs are explicit and tracked in issues.

## High-Volume Collaboration Rules

When PR traffic is high (especially with AI-assisted contributions), these rules keep quality and throughput stable:

- **One concern per PR**: avoid mixing refactor + feature + infra in one change.
- **Small PRs first**: prefer PR size `XS/S/M`; split large work into stacked PRs.
- **Template is mandatory**: complete every section in `.github/pull_request_template.md`.
- **Explicit rollback**: every PR must include a fast rollback path.
- **Security-first review**: changes in `src/security/`, runtime, gateway, and CI need stricter validation.
- **Risk-first triage**: use labels (`risk: high`, `risk: medium`, `risk: low`) to route review depth.
- **Privacy-first hygiene**: redact/anonymize sensitive payloads and keep tests/examples neutral and project-scoped.
- **Identity normalization**: when identity traits are unavoidable, use ZeroClaw/project-native roles instead of personal or real-world identities.
- **Supersede hygiene**: if your PR replaces an older open PR, add `Supersedes #...` and request maintainers close the outdated one.

Full maintainer workflow: [`docs/pr-workflow.md`](docs/pr-workflow.md).
CI workflow ownership and triage map: [`docs/ci-map.md`](docs/ci-map.md).
Reviewer operating checklist: [`docs/reviewer-playbook.md`](docs/reviewer-playbook.md).

## Agent Collaboration Guidance

Agent-assisted contributions are welcome and treated as first-class contributions.

For smoother agent-to-agent and human-to-agent review:

- Keep PR summaries concrete (problem, change, non-goals).
- Include reproducible validation evidence (`fmt`, `clippy`, `test`, scenario checks).
- Add brief workflow notes when automation materially influenced design/code.
- Agent-assisted PRs are welcome, but contributors remain accountable for understanding what the code does and what it could affect.
- Call out uncertainty and risky edges explicitly.

We do **not** require PRs to declare an AI-vs-human line ratio.

Agent implementation playbook lives in [`AGENTS.md`](AGENTS.md).

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

## Code Naming Conventions (Required)

Use these defaults unless an existing subsystem pattern clearly overrides them.

- **Rust casing**: modules/files `snake_case`, types/traits/enums `PascalCase`, functions/variables `snake_case`, constants `SCREAMING_SNAKE_CASE`.
- **Domain-first naming**: prefer explicit role names such as `DiscordChannel`, `SecurityPolicy`, `SqliteMemory` over ambiguous names (`Manager`, `Util`, `Helper`).
- **Trait implementers**: keep predictable suffixes (`*Provider`, `*Channel`, `*Tool`, `*Memory`, `*Observer`, `*RuntimeAdapter`).
- **Factory keys**: keep lowercase and stable (`openai`, `discord`, `shell`); avoid adding aliases without migration need.
- **Tests**: use behavior-oriented names (`subject_expected_behavior`) and neutral project-scoped fixtures.
- **Identity-like labels**: if unavoidable, use ZeroClaw-native identifiers only (`ZeroClawAgent`, `zeroclaw_user`, `zeroclaw_node`).

## Architecture Boundary Rules (Required)

Keep architecture extensible and auditable by following these boundaries.

- Extend features via trait implementations + factory registration before considering broad refactors.
- Keep dependency direction contract-first: concrete integrations depend on shared traits/config/util, not on other concrete integrations.
- Avoid cross-subsystem coupling (provider ↔ channel internals, tools mutating security/gateway internals directly, etc.).
- Keep responsibilities single-purpose by module (`agent` orchestration, `channels` transport, `providers` model I/O, `security` policy, `tools` execution, `memory` persistence).
- Introduce shared abstractions only after repeated stable use (rule-of-three) and at least one current caller.
- Treat `src/config/schema.rs` keys as public contract; document compatibility impact, migration steps, and rollback path for changes.

## Naming and Architecture Examples (Bad vs Good)

Use these quick examples to align implementation choices before opening a PR.

### Naming examples

- **Bad**: `Manager`, `Helper`, `doStuff`, `tmp_data`
- **Good**: `DiscordChannel`, `SecurityPolicy`, `send_message`, `channel_allowlist`

- **Bad test name**: `test1` / `works`
- **Good test name**: `allowlist_denies_unknown_user`, `provider_returns_error_on_invalid_model`

- **Bad identity-like label**: `john_user`, `alice_bot`
- **Good identity-like label**: `ZeroClawAgent`, `zeroclaw_user`, `zeroclaw_node`

### Architecture boundary examples

- **Bad**: channel implementation directly imports provider internals to call model APIs.
- **Good**: channel emits normalized `ChannelMessage`; agent/runtime orchestrates provider calls via trait contracts.

- **Bad**: tool mutates gateway/security policy directly from execution path.
- **Good**: tool returns structured `ToolResult`; policy enforcement remains in security/runtime boundaries.

- **Bad**: adding broad shared abstraction before any repeated caller.
- **Good**: keep local logic first; extract shared abstraction only after stable rule-of-three evidence.

- **Bad**: config key changes without migration notes.
- **Good**: config/schema changes include defaults, compatibility impact, migration steps, and rollback guidance.

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

- [ ] PR template sections are completed (including security + rollback)
- [ ] `cargo fmt --all -- --check` — code is formatted
- [ ] `cargo clippy --all-targets -- -D warnings` — no warnings
- [ ] `cargo test` — all tests pass locally or skipped tests are explained
- [ ] New code has inline `#[cfg(test)]` tests
- [ ] No new dependencies unless absolutely necessary (we optimize for binary size)
- [ ] README updated if adding user-facing features
- [ ] Follows existing code patterns and conventions
- [ ] Follows code naming conventions and architecture boundary rules in this guide
- [ ] No personal/sensitive data in code/docs/tests/fixtures/logs/examples/commit messages
- [ ] Test names/messages/fixtures/examples are neutral and project-focused
- [ ] Any required identity-like wording uses ZeroClaw/project-native labels only

## Commit Convention

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add Anthropic provider
feat(provider): add Anthropic provider
fix: path traversal edge case with symlinks
docs: update contributing guide
test: add heartbeat unicode parsing tests
refactor: extract common security checks
chore: bump tokio to 1.43
```

Recommended scope keys in commit titles:

- `provider`, `channel`, `memory`, `security`, `runtime`, `ci`, `docs`, `tests`

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
- **Privacy**: Redact/anonymize all personal data and sensitive identifiers before posting logs/payloads

## Maintainer Merge Policy

- Require passing `CI Required Gate` before merge.
- Require docs quality checks when docs are touched.
- Require review approval for non-trivial changes.
- Require CODEOWNERS review for protected paths.
- Use risk labels to determine review depth, scope labels (`core`, `provider`, `channel`, `security`, etc.) to route ownership, and module labels (`<module>:<component>`, e.g. `channel:telegram`, `provider:kimi`, `tool:shell`) to route subsystem expertise.
- Contributor tier labels are auto-applied on PRs and issues by merged PR count: `experienced contributor` (>=10), `principal contributor` (>=20), `distinguished contributor` (>=50). Treat them as read-only automation labels; manual edits are auto-corrected.
- Prefer squash merge with conventional commit title.
- Revert fast on regressions; re-land with tests.

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
