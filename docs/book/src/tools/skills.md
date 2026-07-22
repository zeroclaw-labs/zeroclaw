# Skills

Skills are reusable instructions and optional tool definitions that ZeroClaw can load into an agent session. Use them for repeatable workflows such as code review checklists, deployment runbooks, support playbooks, or domain-specific tool wrappers.

Skills live in one of three locations:

- Per-agent workspace skills under `<install>/agents/<alias>/workspace/skills/<name>/`.
- Shared skill bundles under `<install>/shared/skills/<bundle>/<name>/`. Agents load these when their config lists the bundle in `agents.<alias>.skill_bundles`.
- The global skill directory under `<install>/data/skills/<name>/`. The CLI can install there as a fallback, but agents do not load global skills automatically.

Use bundles for skills an agent should load during runtime. A bundle is configured under `[skill_bundles.<alias>]`; when its `directory` is omitted, ZeroClaw resolves it to `<install>/shared/skills/<alias>/`.

```text
<install>/shared/skills/<bundle>/<name>/
```

For hand-authored local skills, use `SKILL.md` or `SKILL.toml`. Use `SKILL.md` for instructions plus simple metadata. Use `SKILL.toml` when the skill needs structured prompts or tool definitions. ZeroClaw also understands `manifest.toml` for registry-style skill packages, but `SKILL.md` and `SKILL.toml` are the recommended local authoring formats.

To distribute a set of skills as a signed, versioned, installable package, see [Skill bundles](./skill-bundles.md).

## Create a Markdown skill

Create a bundle, then scaffold an instruction-only skill into it:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills bundle add ops
zeroclaw skills add release-check \
  --bundle ops \
  --description "Check release readiness before tagging" \
  --edit
```

</div>

The `skills add` command writes `SKILL.md` under the resolved bundle directory and opens it in your editor. Replace the generated instructions with the workflow you want the agent to follow:

```markdown
# Release check

Review the release notes, changelog, version tags, and migration notes before confirming that a release is ready.
```

The directory name becomes the skill name. ZeroClaw uses the first non-heading paragraph as the description when no frontmatter description is present.

`SKILL.md` also supports simple frontmatter for metadata:

```markdown
---
name: release-check
description: Check release readiness before tagging
version: 0.1.0
author: zeroclaw_user
tags: [release, docs]
---

# Release check

Review the release notes, changelog, version tags, and migration notes before confirming that a release is ready.
```

Supported frontmatter fields are `name`, `description`, `version`, `author`, and `tags`.

## Create a TOML skill

A skill can also be a structured TOML manifest (`SKILL.toml`). The `[skill]` table requires `name` and `description`; `version` defaults to `0.1.0` when omitted; `author`, `tags`, and `prompts` are optional. Tool entries may use `kind = "shell"`, `kind = "http"`, or `kind = "script"`. Keep tool descriptions narrow and concrete so the model knows when to use them.

### Slash command options and localizations

A skill tagged `slash` is surfaced as a chat-channel slash command (e.g. Discord `/search`). It may declare typed `[[skill.slash_options]]`; a skill that declares none falls back to a single required free-text input. Both the command description and each option description accept an optional `description_localizations` map keyed by locale code. Unknown or unsupported locale codes are dropped with a warning rather than failing registration, so a typo never wedges command registration.

```toml
[skill]
name = "search"
description = "Search the web"
tags = ["slash"]
# Localized command descriptions, keyed by locale code.
description_localizations = { fr = "Rechercher sur le web", ja = "ウェブを検索" }

[[skill.slash_options]]
name = "query"
description = "The search query"
type = "string"
required = true
# Localized option descriptions, same form.
description_localizations = { fr = "La requête de recherche" }
```

## Manage installed skills

List the full inventory:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills list
```

</div>

List exactly what one agent loads at runtime:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills list --agent default
```

</div>

List one bundle directly:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills list --bundle ops
```

</div>

Audit an installed skill or a local skill directory:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills audit release-check
zeroclaw skills audit ./release-check
```

</div>

Install a skill from a local directory, Git URL, or registry name:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills install ./release-check --bundle ops
zeroclaw skills install https://example.com/zeroclaw-release-check.git --bundle ops
zeroclaw skills install release-check --agent default
```

</div>

Install one skill by name from a Git catalog repository (a repo whose skills live under `skills/<name>/`):

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills install https://github.com/vercel-labs/skills --skill find-skills
```

</div>

Install destination precedence is:

1. Explicit `--bundle <alias>`.
2. The target agent's single assigned bundle. `--agent <alias>` chooses the target agent; when omitted, ZeroClaw uses the active runtime agent.
3. The global directory under `<install>/data/skills/`.

If the target agent has multiple bundles, pass `--bundle` so the destination is unambiguous. If ZeroClaw falls back to the global directory, the skill is installed and listed, but no agent loads it automatically. Attach it to a bundle to make it available at runtime.

Remove an installed skill:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills remove release-check --bundle ops
zeroclaw skills remove release-check --agent default
```

</div>

Removing from a bundle archives the skill directory so it can be recovered. Removing from the global directory deletes the global copy after the existing path-containment checks pass.

Run `TEST.sh` validation for one skill, or omit the name to test all installed skills:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills test release-check
zeroclaw skills test --verbose
```

</div>

`zeroclaw skills test` runs the skill's `TEST.sh` file when one exists. Inspect `TEST.sh` before running tests from a skill source you do not already trust.

If `zeroclaw skills list` shows a skill but the agent does not use it, check the runtime view:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills list --agent default
```

</div>

When the skill appears only in the global group, install it into a bundle and ensure the agent lists that bundle in `agents.<alias>.skill_bundles`.

For a worked example that turns a built-in tool into a reusable operator workflow, see [using relationship memory from skills](./relationship-memory-skill-template.md).

## Prompt-triggered capability suggestions

ZeroClaw can optionally suggest an installable skill capability when a submitted prompt clearly names something that exists in cached registry metadata but is not installed. The server-side path runs after submission and before the normal LLM turn. It only returns a suggestion; it does not install the skill, enable it, write memory, or treat the skill body as global instructions.

Enable it via the `skills` config (gateway, zerocode, or `zeroclaw config set`). The suggestion matcher uses installed skill names and cached registry metadata such as names, aliases, and frontmatter. It intentionally avoids matching unapproved skill bodies. Plugin/package-level discovery remains follow-up scope until the plugin registry search/install surface is available. Exact composer-time suggestions while the user is still typing require ACP, gateway, or client UI support and are outside this server-only path.

## Script safety

ZeroClaw audits skills before loading or installing them. Script-like files such as `.sh`, `.bash`, `.ps1`, and files with shell shebangs are blocked by default.

If you intentionally use script-bearing skills, enable `skills.allow_scripts`. Keep this disabled unless you trust the skill source and have reviewed what the scripts do.

For Python-specific execution patterns, interpreter policy, and native versus Docker trade-offs, see [Running Python skills](./python-skills.md).

## Loading community skills

Community open-skills loading is opt-in via the `skills` config. When enabled, ZeroClaw loads skills from the configured `open_skills_dir`, or from `$HOME/open-skills` when no directory is set. If that directory does not exist, ZeroClaw may clone the community open-skills repository; if it does exist and is a git checkout, ZeroClaw may pull updates. Enable this only for community sources you trust, or point `open_skills_dir` at a reviewed local copy.

## Advanced config

The default prompt injection mode is `full`, which includes full skill instructions in the system prompt. Use `compact` to keep only compact metadata in context and load skill details on demand:

## Autonomous skill creation

After a successful multi-step task (at least two tool calls), ZeroClaw can persist the execution as a reusable skill. This is **off by default** and opt-in:

```toml
[skills.skill_creation]
enabled = true              # off by default
max_skills = 500            # LRU cap: oldest auto-generated skill is evicted past this
similarity_threshold = 0.85 # embedding-dedup cutoff; near-duplicate tasks are skipped
```

By default each created skill is a deterministic `SKILL.toml` generated directly from the tool-call trace; no model call is involved.

### Reflection (`SKILL.md` synthesis)

With reflection enabled, ZeroClaw instead asks the agent's configured model provider to synthesize a canonical [`SKILL.md`](#create-a-markdown-skill) from a **bounded** slice of the execution (the task, the tool-call trace, and the final answer). Every input is independently truncated to a configured character budget so a large execution can never produce an unbounded reflection request:

```toml
[skills.skill_creation]
enabled = true
reflection_enabled = true   # opt-in; requires enabled = true
max_task_chars = 1000           # task description budget
max_tool_trace_chars = 4000     # tool-call trace budget
max_final_answer_chars = 2000   # final assistant answer budget
```

If the reflection call fails (provider error, malformed output, or an empty body), ZeroClaw falls back to the deterministic `SKILL.toml` path, so enabling reflection never leaves a skill un-created. Reflected skills are stamped with the `zeroclaw-auto` author and participate in the same dedup and LRU eviction as `SKILL.toml` skills.

Because reflection forwards turn content to the model provider, the task, the tool-call trace, and the final answer are each scanned for credential-shaped values (API keys, tokens, AWS credentials, PEM private keys, JWTs, database connection URLs, and high-entropy secrets) and redacted **before** the prompt is composed and sent, using the same outbound-content guardrail ZeroClaw applies to channel responses. Redaction runs in-process ahead of the request, so a secret that appears in a tool argument or the final answer is replaced with a `[REDACTED_…]` marker rather than reaching the provider.

> **Reflection vs. skill improvement.** Reflection (`[skills.skill_creation] reflection_enabled`) *creates a new skill* from a completed execution trace. The `[skills.skill_improvement]` background review fork is a separate feature that *patches existing skills* after they are used. They can be enabled independently.

## See also

- [Tools overview](./overview.md)
- [Using relationship memory from skills](./relationship-memory-skill-template.md)
- [Security overview](../security/overview.md)
- [Tool receipts](../security/tool-receipts.md)
