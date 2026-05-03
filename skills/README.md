# skills/

Canonical home for first-party ZeroClaw skills.

This directory **is** the registry. End users install with:

```sh
zeroclaw skills install <name>
```

The CLI sparse-checks out the `skills/` subtree from `zeroclaw-labs/zeroclaw` — currently ~150 KB total versus the full source tree's hundreds of MB — and installs the requested skill into `~/.zeroclaw/workspace/skills/<name>/`. Skills are not bundled into the binary and not mirrored to a separate registry repo: there is one source of truth, and you're looking at it.

## Layout

```
skills/
├── README.md                  ← this file
├── hello-zeroclaw/            ← every skill is its own directory
│   └── SKILL.md
└── <skill-name>/
    ├── SKILL.md               ← required: prompt + YAML frontmatter
    ├── SKILL.toml             ← optional: structured manifest, including tool definitions
    ├── README.md              ← optional: human-facing docs
    └── scripts/               ← optional, gated by `skills.allow_scripts = true`
```

The directory name **is** the install name. `zeroclaw skills install hello-zeroclaw` resolves to `skills/hello-zeroclaw/`.

## Required: `SKILL.md`

YAML frontmatter + markdown body. Frontmatter fields the runtime parses:

| Field | Required | Notes |
|---|---|---|
| `name` | yes | Display name. Can differ from directory name (the directory wins for install resolution). |
| `description` | yes | One-line or `>-`-folded multi-line description. Shown in `zeroclaw skills list`. |
| `version` | recommended | Quoted string like `"1.0.0"`. Defaults to `"0.1.0"` if omitted. |
| `author` | recommended | Name or org. |
| `tags` | optional | YAML list of dashes (`- tag`). Bracket arrays (`[a, b]`) are not parsed. |

The body is what gets injected into the agent's system prompt. Keep it short, action-oriented, and free of fabricated capability claims.

Reference the `Skill` and `SkillMeta` structs at [`crates/zeroclaw-runtime/src/skills/mod.rs:36-91`](../crates/zeroclaw-runtime/src/skills/mod.rs#L36-L91) for the full schema.

## Optional: `SKILL.toml`

Use this when your skill needs structured fields beyond what frontmatter handles cleanly — most notably **tools**. Format:

```toml
[skill]
name = "my-skill"
description = "..."
version = "1.0.0"
author = "ZeroClaw Labs"
tags = ["category", "subcategory"]
prompts = ["Optional inline prompt strings"]

[[tools]]
name = "my_tool"
description = "..."
kind = "shell"        # "shell" | "http" | "script"
command = "echo hello"
```

When both `SKILL.md` and `SKILL.toml` are present, **TOML takes precedence** for fields they share. The runtime emits a warning if the two disagree on `name` or `description`.

## Tool authors: apply the output-fidelity rule

If your skill defines tools (under `[[tools]]` in `SKILL.toml`), every tool's stdout/output must end with a fidelity footer that names the data source and lists the fields actually returned. This blocks LLM hallucination of fields the tool didn't produce. Example:

```
---
Data source: <provider> (<endpoint or scope>).
Fields returned: <comma-separated list of fields actually present>.
Do not infer, estimate, or add fields that are not in this output.
```

The dashes are required as a structural boundary.

## Local testing

From the repo root:

```sh
cargo run -- skills install ./skills/hello-zeroclaw
cargo run -- skills list
cargo run -- skills audit hello-zeroclaw
```

This copies your skill into `~/.zeroclaw/workspace/skills/hello-zeroclaw/`, runs the security audit, and registers it for the agent runtime to pick up.

To remove:

```sh
cargo run -- skills remove hello-zeroclaw
```

## How the bare-name install resolver works

When a user runs `zeroclaw skills install <name>` (a bare name, not a path or URL):

1. The CLI calls [`install_registry_skill_source`](../crates/zeroclaw-runtime/src/skills/mod.rs#L1593) which calls [`ensure_skills_registry`](../crates/zeroclaw-runtime/src/skills/mod.rs#L1556).
2. If the local cache (`~/.zeroclaw/workspace/skills-source/`) is missing, it `git clone --depth=1 --filter=blob:none --sparse https://github.com/zeroclaw-labs/zeroclaw <cache>` and then `git sparse-checkout set skills`. Only the `skills/` subtree is materialised.
3. If the cache exists and is older than 24h, it runs `git pull --ff-only` (which respects sparse-checkout).
4. The CLI reads `<cache>/skills/<name>/` and copies it through `install_local_skill_source`, which runs the security audit before accepting the skill.

The override is `skills.registry_url` in `~/.zeroclaw/config.toml` if you want to test against a fork.

## Other supported install sources

- **Local path:** `zeroclaw skills install ./path/to/skill/`
- **Git URL:** `zeroclaw skills install https://github.com/<org>/<repo>` (clones and installs)
- **ClawHub URL:** `zeroclaw skills install https://clawhub.ai/skills/<name>`

Bare names always resolve through the main repo. The other sources let users install skills outside this registry.

## Submission checklist

Before opening a PR with a new skill:

1. Skill has a `SKILL.md` with required frontmatter (`name`, `description`).
2. Directory name matches the canonical install name (kebab-case, no spaces).
3. `cargo run -- skills install ./skills/<name>` succeeds locally and the audit reports zero findings.
4. `cargo run -- skills list` shows the new skill with the expected description.
5. If the skill defines tools, every tool's output ends with the fidelity footer.
6. No secrets, credentials, hardcoded paths, or PII in any file.
7. Scripts (`*.sh`, `*.bash`, `*.ps1`) are only present when genuinely needed; skills containing scripts are skipped at runtime unless the user has set `skills.allow_scripts = true`. Document that requirement in the skill's own README if applicable.

## Configuration

Three top-level knobs control how skills load into the agent. Edit them via `zeroclaw config set` or the gateway's config CRUD endpoints.

| Setting | Default | Effect |
|---|---|---|
| `skills.enabled` | `true` | Master toggle. When `false`, no skills load into the agent regardless of what's installed on disk. |
| `skills.disabled` | `[]` | Per-skill blocklist by canonical name. E.g. `["chatty-skill"]` skips `chatty-skill` even if it's installed. Useful for disabling without uninstalling. |
| `skills.prompt_injection_mode` | `"compact"` | `"compact"`: only skill names + descriptions go in the system prompt; bodies load on demand via `ReadSkillTool`. Keeps per-turn token cost flat. `"full"`: every installed skill's full body inlined into the system prompt every turn (legacy behavior). |

### Token math (why compact is the default)

| Skills installed | `full` mode | `compact` mode |
|---|---|---|
| 5 | ~6,250 tokens/turn | ~250 tokens/turn |
| 20 | ~25,000 tokens/turn | ~1,000 tokens/turn |
| 50 | ~62,500 tokens/turn | ~2,500 tokens/turn |
| 100 | ~125,000 tokens/turn | ~5,000 tokens/turn |

In compact mode you can install ~100 skills and still spend less than 3% of a 200K context on the catalog. Bodies only show up when the agent actually uses a skill, for that turn only.

### Examples

```sh
# Disable a noisy skill without removing it
zeroclaw config set skills.disabled '["overly-eager-skill"]'

# Switch back to full-body injection for testing/legacy behavior
zeroclaw config set skills.prompt_injection_mode full

# Kill switch — load no skills at all
zeroclaw config set skills.enabled false
```

## Automation

[`.github/workflows/skills-validate.yml`](../.github/workflows/skills-validate.yml) runs on every PR that touches `skills/**`. It builds `zeroclaw`, runs `zeroclaw skills audit ./skills/<name>` for each skill, and verifies the install/list/remove round-trip. Audit failures block merge.

## Anti-patterns

- **Do not** depend on local environment paths (`/Users/...`, `~/`) inside `SKILL.md` — the same skill installs to other users' machines.
- **Do not** ship secrets, API keys, OAuth tokens, or anything in `enc2:` form.
- **Do not** request `skills.allow_scripts = true` casually. Most skills should be pure prompts + HTTP tool definitions.
- **Do not** invent capabilities the agent doesn't have. The skill body is loaded into the system prompt verbatim — false claims become hallucinations.
- **Do not** localize `SKILL.md` content with Fluent (`fl!()`). Skill bodies are agent-facing English.

## Out of scope (today)

- **Versioned distribution.** Skills are pulled from the main repo's `master` branch. Per-version pinning is not yet supported.
- **Binary bundling.** A future opt-in `bundle-skills` cargo feature may exist for offline/air-gapped deployments. The default release ships with no skills bundled — they download on demand.
- **A separate community-tier registry.** Right now first-party and community skills both live here. If submission volume grows, a second-tier `zeroclaw-skills-community` repo with a lighter review bar is a possible future split — but today the consolidation wins on simplicity.
