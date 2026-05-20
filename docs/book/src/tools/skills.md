# Skills

Skills are reusable instructions and optional tool definitions that ZeroClaw can load into an agent session. Use them for repeatable workflows such as code review checklists, deployment runbooks, support playbooks, or domain-specific tool wrappers.

Skills live in the workspace under `skills/<name>/`. With the default workspace this is:

```text
~/.zeroclaw/workspace/skills/<name>/
```

For hand-authored local skills, use `SKILL.md` or `SKILL.toml`. Use `SKILL.md` for instructions plus simple metadata. Use `SKILL.toml` when the skill needs structured prompts or tool definitions. ZeroClaw also understands `manifest.toml` for registry-style skill packages, but `SKILL.md` and `SKILL.toml` are the recommended local authoring formats.

## Create a Markdown skill

A minimal instruction-only skill can be just a Markdown file:

```bash
mkdir -p ~/.zeroclaw/workspace/skills/release-check
$EDITOR ~/.zeroclaw/workspace/skills/release-check/SKILL.md
```

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

Here is the same skill as a structured TOML manifest:

```toml
[skill]
name = "release-check"
description = "Check release readiness before tagging"
version = "0.1.0"
author = "zeroclaw_user"
tags = ["release", "docs"]
prompts = [
  "Review the release notes, changelog, version tags, and migration notes before confirming that a release is ready."
]

[[tools]]
name = "show_latest_tag"
description = "Print the latest local git tag"
kind = "shell"
command = "git describe --tags --abbrev=0"
```

The `[skill]` table requires `name` and `description`. `version` defaults to `0.1.0` when omitted. `author`, `tags`, and `prompts` are optional.

Tool entries may use `kind = "shell"`, `kind = "http"`, or `kind = "script"`. Keep tool descriptions narrow and concrete so the model knows when to use them.

## Manage installed skills

List installed skills:

```bash
zeroclaw skills list
```

Audit an installed skill or a local skill directory:

```bash
zeroclaw skills audit release-check
zeroclaw skills audit ./release-check
```

Install a skill from a local directory, Git URL, registry name, or ClawHub source:

```bash
zeroclaw skills install ./release-check
zeroclaw skills install https://example.com/zeroclaw-release-check.git
zeroclaw skills install release-check
zeroclaw skills install clawhub:release-check
```

Remove an installed skill:

```bash
zeroclaw skills remove release-check
```

Run `TEST.sh` validation for one skill, or omit the name to test all installed skills:

```bash
zeroclaw skills test release-check
zeroclaw skills test --verbose
```

`zeroclaw skills test` runs the skill's `TEST.sh` file when one exists. Inspect `TEST.sh` before running tests from a skill source you do not already trust.

## Prompt-triggered capability suggestions

ZeroClaw can optionally suggest an installable skill capability when a submitted prompt clearly names something that exists in cached registry metadata but is not installed. The server-side path runs after submission and before the normal LLM turn. It only returns a suggestion; it does not install the skill, enable it, write memory, or treat the skill body as global instructions.

Enable it in config:

```toml
[skills.install_suggestions]
enabled = true
```

The suggestion matcher uses installed skill names and cached registry metadata such as names, aliases, and frontmatter. It intentionally avoids matching unapproved skill bodies. Plugin/package-level discovery remains follow-up scope until the plugin registry search/install surface is available. Exact composer-time suggestions while the user is still typing require ACP, gateway, or client UI support and are outside this server-only path.

## Script safety

ZeroClaw audits skills before loading or installing them. Script-like files such as `.sh`, `.bash`, `.ps1`, and files with shell shebangs are blocked by default.

If you intentionally use script-bearing skills, enable them in the ZeroClaw config:

```toml
[skills]
allow_scripts = true
```

Keep this disabled unless you trust the skill source and have reviewed what the scripts do.

## Loading community skills

Community open-skills loading is opt-in:

```toml
[skills]
open_skills_enabled = true
```

When enabled, ZeroClaw loads skills from the configured `open_skills_dir`, or from `$HOME/open-skills` when no directory is set. If that directory does not exist, ZeroClaw may clone the community open-skills repository; if it does exist and is a git checkout, ZeroClaw may pull updates. Enable this only for community sources you trust, or point `open_skills_dir` at a reviewed local copy.

## Advanced config

The default prompt injection mode is `full`, which includes full skill instructions in the system prompt. Use `compact` to keep only compact metadata in context and load skill details on demand:

```toml
[skills]
prompt_injection_mode = "compact"
```

## See also

- [Tools overview](./overview.md)
- [Security overview](../security/overview.md)
- [Tool receipts](../security/tool-receipts.md)
