# Stealinglight/ZeroClaw Fork — Branch Strategy & Contribution Plan

**Fork:** https://github.com/Stealinglight/zeroclaw
**Upstream:** https://github.com/zeroclaw-labs/zeroclaw
**Maintainer:** Stealinglight

---

## Branch Strategy

```
upstream/master (zeroclaw-labs/zeroclaw)
    |
    |  git fetch upstream && git checkout master && git merge upstream/master --ff-only
    v
master ──────────── mirrors upstream exactly (never commit directly)
    |
    ├── feat/* ──── contribution branches (PR to upstream)
    |
    └── stealinglight ── personal build (custom features + upstream)
                         rebases on master regularly
```

### Branch Purposes

| Branch | Purpose | Updates from | PRs to |
|--------|---------|-------------|--------|
| `master` | Read-only mirror of upstream | `upstream/master` (fast-forward merge) | Never |
| `feat/*` | Upstream contribution work | Created from `upstream/master` | `zeroclaw-labs/zeroclaw:master` |
| `stealinglight` | Personal build for deployment | Rebases on `master` after sync | Never (personal use) |

### Sync Workflow

```bash
# 1. Sync master with upstream
git fetch upstream
git checkout master
git merge upstream/master --ff-only
git push origin master

# 2. Rebase personal branch on updated master
git checkout stealinglight
git rebase master
git push origin stealinglight --force-with-lease

# 3. Build and deploy
cargo build --release
```

### Contribution Workflow

```bash
# 1. Create feature branch from latest upstream
git fetch upstream
git checkout -b feat/my-feature upstream/master

# 2. Implement, test, commit
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test

# 3. Push and PR
git push -u origin feat/my-feature
gh pr create --repo zeroclaw-labs/zeroclaw --base master

# 4. After merge, feature flows into master via sync, then into stealinglight via rebase
```

### When a Feature Gets Merged Upstream

Once a `feat/*` PR is merged into `zeroclaw-labs/zeroclaw:master`:
1. The commits are now in `upstream/master`
2. Sync master (step 1 above) brings them into your `master`
3. Rebase `stealinglight` on `master` — the duplicate commits auto-resolve
4. Delete the merged `feat/*` branch: `git branch -d feat/my-feature`

---

## Git Remotes

| Remote | URL | Purpose |
|--------|-----|---------|
| `origin` | `https://github.com/Stealinglight/zeroclaw.git` | Your fork |
| `upstream` | `https://github.com/zeroclaw-labs/zeroclaw.git` | Upstream project |

---

## Contribution Roadmap

### Active

| # | Feature | Branch | PR | Status |
|---|---------|--------|----|--------|
| 1 | Native extended thinking (Anthropic + Bedrock) | `feat/native-extended-thinking-v2` | zeroclaw-labs/zeroclaw#5652 | Open, CI green |

### Planned (in priority order)

| # | Feature | Impact | Effort |
|---|---------|--------|--------|
| 2 | Bedrock streaming (ConverseStream API) | CRITICAL | 3-5 sessions |
| 3 | Bedrock prompt caching + cache token reporting | HIGH | 2-3 sessions |
| 4 | Token usage unification (thinking_tokens) | MEDIUM | 1-2 sessions |
| 5 | PDF document support (Anthropic + Bedrock) | MEDIUM | 2 sessions |
| 6 | Bedrock Guardrails integration | MEDIUM | 2-3 sessions |
| 7 | Anthropic Batches API | LOW-MED | 4+ sessions |

Full roadmap: `.omc/plans/zeroclaw-contribution-roadmap.md`

### Upstream References

- Issue: zeroclaw-labs/zeroclaw#5630 (native extended thinking proposal)
- PR: zeroclaw-labs/zeroclaw#5652 (implementation)
- Contributing guide: `CONTRIBUTING.md` (Track B for provider changes)
- Branch convention: `feat/*` or `fix/*` targeting `master`

---

## Local Development

### Building

```bash
# Debug build (fast compile, slower binary)
cargo build

# Release build (slow compile, optimized binary)
cargo build --release

# Install to PATH
/bin/cp target/release/zeroclaw ~/.cargo/bin/zeroclaw
```

### Testing

```bash
# Quick: changed crates only
cargo test -p zeroclaw-providers -p zeroclaw-runtime -p zeroclaw-config -p zeroclaw-api --lib

# Full: all workspace tests
cargo test --workspace --lib

# Quality gate (matches CI)
cargo fmt --all -- --check
cargo clippy --all-targets -- -D clippy::correctness
```

### Running with Bedrock

```bash
# Requires AWS credentials in ~/.aws/credentials
# Config at ~/.zeroclaw/config.toml
AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... AWS_REGION=us-west-2 \
  zeroclaw agent -m "your message"
```

### Key Config for Native Thinking

```toml
default_provider = "aws-bedrock"
default_model = "us.anthropic.claude-sonnet-4-20250514-v1:0"

[agent.thinking]
default_level = "high"    # or "max" for exhaustive reasoning
native_thinking = true    # uses Anthropic's extended thinking API

# Optional: override budget per level
[agent.thinking.budget_tokens]
# high = 10000   (default)
# max = 50000    (default)
```

---

## Features on `stealinglight` Branch (Not Yet Upstream)

| Feature | Commit | Description |
|---------|--------|-------------|
| Native extended thinking | `719c64a8` | Anthropic + Bedrock providers send `thinking` param with `budget_tokens` |
| Thinking signature round-trip | `5c725f6e` | Interleaved thinking blocks preserved with signatures in tool-use history |
| Thinking resolution helper | `a8eead27` | Deduplicated directive parsing in agent loop |
| Budget safety cap | `51c64833` | MAX_BUDGET_TOKENS=128K, max_tokens > budget_tokens enforcement |

When these get merged upstream, they'll be removed from this list and flow through `master` instead.
