# ZeroClaw Development & Debugging Guide

This document contains instructions for running ZeroClaw in development mode, debugging, and troubleshooting common issues.

## Running ZeroClaw in Debug Mode

### Quick Start (Recommended)

The daemon includes the gateway and all other components, so you typically only need to run one command:

```bash
cd /Users/alaingaldemas/Documents/agentic/zeroclaw
set -a && source .env && set +a
RUST_LOG=debug cargo run --bin zeroclaw -- daemon 2>&1 | tee -a logs/zeroclaw-daemon.log &
```

This starts everything:
- ✅ Gateway on http://127.0.0.1:42617
- ✅ Channel supervisor
- ✅ Heartbeat
- ✅ Scheduler
- ✅ Logs redirected to `./logs/zeroclaw-daemon.log`

**Important**: Use `set -a && source .env && set +a` to export all variables from `.env` to subprocesses. This is critical for transcription features that read `GEMINI_API_KEY` and other provider keys.

**Why this matters:**
- When you run `cargo run`, it spawns a subprocess to compile and execute your code
- By default, `source .env` only exports variables to the current shell session
- `set -a` (auto-export mode) marks all variables for export, so child processes inherit them
- Without this, `GEMINI_API_KEY` won't be available to the transcription code running in the subprocess
- Using `.envrc` with `direnv` is an alternative (auto-exports when entering the directory)

To follow logs in real-time:
```bash
tail -f logs/zeroclaw-daemon.log
```

### Gateway Only (Advanced)

If you only need the gateway without daemon features (no channels, scheduling, etc.):

```bash
cd /Users/alaingaldemas/Documents/agentic/zeroclaw
source .env
RUST_LOG=debug cargo run --bin zeroclaw -- gateway
```

### Environment Variables

ZeroClaw requires environment variables for API keys. There are several ways to provide them:

#### Option 1: Source .env file

```bash
set -a && source .env && set +a
cargo run --bin zeroclaw -- <command>
```

**Important**: The `set -a` command exports all variables defined in `.env` to the environment, making them available to child processes (like `cargo run`). This is required for transcription features to work properly.

**Why `set -a` is necessary in development/debug:**
- The `.env` file typically contains API keys (GEMINI_API_KEY, GROQ_API_KEY, etc.)
- When cargo runs your code, it does so in a **child process**
- Without `set -a`, the child process won't see the variables from `.env` even if the parent shell does
- This is especially important for transcription because it runs async HTTP requests to Google's API
- Using `.envrc` with direnv is recommended as an alternative that handles this automatically

#### Option 2: Set variables directly

```bash
export GEMINI_API_KEY="your-api-key-here"
cargo run --bin zeroclaw -- <command>
```

### Key Environment Variables

| Variable | Usage | Required For |
|----------|-------|--------------|
| `GEMINI_API_KEY` | Google Gemini API key | Gemini provider |
| `ZEROCLAW_API_KEY` | Generic API key override | All providers (overrides config) |
| `API_KEY` | Fallback generic key | All providers |
| `PROVIDER` | Default provider | Setting default provider |

**Note**: `ZEROCLAW_API_KEY` is detected by `zeroclaw doctor` and will make the "no api_key set" warning disappear. `GEMINI_API_KEY` works at runtime but is not detected by the doctor command.

### Port Management

If you get "Address already in use" error:

```bash
# Find the process using the port
lsof -i :42617

# Kill it
lsof -ti :42617 | xargs kill -9
```

### Health Check

Run `zeroclaw doctor` to verify everything is working:

```bash
source .env && cargo run --bin zeroclaw -- doctor
```

Expected output should show:
- ✅ config file
- ✅ provider valid
- ✅ daemon running (heartbeat fresh)
- ✅ scheduler healthy

### Common Warnings

| Warning | Explanation | Action |
|---------|-------------|--------|
| `⚠️ no api_key set` | Doctor checks config.toml only, not env vars | Ignore if using GEMINI_API_KEY in .env, or add ZEROCLAW_API_KEY |
| `⚠️ no channels configured` | No messaging channels set up | Run `zeroclaw onboard` if needed |
| `❌ state file not found` | Daemon not running | Start with `zeroclaw daemon` |

## Understanding the Warning: "no api_key set"

The `zeroclaw doctor` command checks only `config.api_key` in config.toml — it does NOT check environment variables.

**However, your API key will still work!** The provider (e.g., Gemini) reads `GEMINI_API_KEY` directly at runtime via `std::env::var()`.

This is a false positive. To make the warning disappear:

1. Add `api_key` to config.toml (with encryption):
   ```bash
   zeroclaw config set api_key "your-key"
   ```

2. Or add `ZEROCLAW_API_KEY` to your .env (this IS detected by doctor):
   ```
   ZEROCLAW_API_KEY=your-key
   ```

## Secure API Key Storage

For production, use ZeroClaw's encrypted SecretStore:

1. The secret is encrypted with ChaCha20-Poly1305
2. Encryption key stored in `~/.zeroclaw/.secret_key` (permissions 0600)
3. Config stores only ciphertext (format: `enc2:...`)

Enable in config.toml:
```toml
[secrets]
encrypt = true
```

Then set your API key — it will be automatically encrypted:
```bash
zeroclaw config set api_key "your-gemini-key"
```

## Log Files

Debug logs are redirected to `./logs/zeroclaw-daemon.log` when running the daemon with `tee`. For other processes, check:
- Terminal output where you started the command
- `zeroclaw_agent.log` in the workspace directory

## Testing Configuration Changes

When modifying config.toml or testing fixes, follow this workflow:

### 1. Kill existing daemon
```bash
lsof -ti :42617 | xargs kill -9
```

### 2. Start daemon in background with logs
```bash
cd /Users/alaingaldemas/Documents/agentic/zeroclaw
set -a && source .env && set +a
RUST_LOG=debug cargo run --bin zeroclaw -- daemon 2>&1 | tee -a logs/zeroclaw-daemon.log &
```

### 3. Verify daemon started successfully
Look for these indicators in the logs:
- ✅ Config loaded
- ✅ ZeroClaw daemon started
- ✅ Gateway listening on http://127.0.0.1:42617
- ✅ Telegram channel listening for messages

### 4. Follow logs in real-time
```bash
tail -f logs/zeroclaw-daemon.log
```
Or open the log file directly in the editor.

## Planning and Architecture Guidelines

**When creating plans, technical documents, or architectural proposals, you MUST consult and follow the relevant protocol files.**

### Priority Order:

1. **`AGENTS.md`** - Primary protocol for coding agents (Codex/Cline)
   - Section 0: Session targets and clean worktree requirements
   - Section 6: Agent workflow and validation
   - Section 6.1: Branch/PR flow
   - Section 6.2: Worktree workflow
   - Section 8: Validation matrix

2. **`CLAUDE.md`** - Architecture and engineering principles
   - Section 3: Engineering Principles (KISS, YAGNI, SRP, etc.)
   - Section 6.4: Architecture Boundary Contract
   - Section 7: Change Playbooks (how to properly add providers, channels, tools)

3. **`CONTRIBUTING.md`** - Contribution guidelines and PR requirements

### Key Rules Before Proposing Any Plan:

1. **Always use a clean worktree** - Create a new git worktree for each task (see AGENTS.md Section 0.1)
2. **Run `search_files`** - Verify the feature doesn't already exist
3. **Check trait definitions** - Inspect `src/providers/traits.rs`, `src/channels/traits.rs`, `src/tools/traits.rs`
4. **Use generic references** - Say "Any Configured Provider" not "OpenAI"
5. **One concern per PR** - Avoid mixed feature+refactor+infra patches
6. **Validate by risk tier** - Follow validation matrix in AGENTS.md Section 8
7. **Document impact** - Include behavior changes, risks, and rollback plan

### Documentation System Rules

When modifying docs, follow `docs/i18n-guide.md`:
- Keep multilingual entry-point parity (en, zh-CN, ja, ru, fr, vi, el)
- Update all locale hubs when changing navigation
- Follow the i18n completion checklist before merge

## Code Language Guidelines

### Comments

- **Always write comments in English** in the source code
- Comments should be clear, concise, and explain the "why" not the "what"
- Use proper English grammar and spelling

### Strings and User-Facing Text

- **All strings in code must be in English (UTF-8)**
- This includes error messages, log messages, UI text, and any user-facing content
- Do not use non-English characters or localized strings in the source code
- Localization should be handled through separate i18n files, not inline in the code

### Rationale

- English is the standard language for code collaboration in open source projects
- Keeping strings in English ensures consistency across the codebase
- Facilitates contributions from international developers
- Makes debugging and code review easier
