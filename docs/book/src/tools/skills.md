# Skills

Skills are reusable instructions and optional tool definitions that ZeroClaw can load into an agent session. Use them for repeatable workflows such as code review checklists, deployment runbooks, support playbooks, or domain-specific tool wrappers.

Skills live in the workspace under `skills/<name>/`. With the default workspace this is:

```text
~/.zeroclaw/workspace/skills/<name>/
```

For hand-authored local skills, use `SKILL.md` or `SKILL.toml`. Use `SKILL.md` for instructions plus simple metadata. Use `SKILL.toml` when the skill needs structured prompts or tool definitions. ZeroClaw also understands `manifest.toml` for registry-style skill packages, but `SKILL.md` and `SKILL.toml` are the recommended local authoring formats.

## Create a Markdown skill

A minimal instruction-only skill can be just a Markdown file:

<div class="os-tabs-src">

#### sh

```sh
mkdir -p ~/.zeroclaw/workspace/skills/release-check
$EDITOR ~/.zeroclaw/workspace/skills/release-check/SKILL.md
```

</div>

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

List installed skills:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills list
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

Install a skill from a local directory, Git URL, registry name, or ClawHub source:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills install ./release-check
zeroclaw skills install https://example.com/zeroclaw-release-check.git
zeroclaw skills install release-check
zeroclaw skills install clawhub:release-check
```

</div>

Remove an installed skill:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills remove release-check
```

</div>

Run `TEST.sh` validation for one skill, or omit the name to test all installed skills:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills test release-check
zeroclaw skills test --verbose
```

</div>

`zeroclaw skills test` runs the skill's `TEST.sh` file when one exists. Inspect `TEST.sh` before running tests from a skill source you do not already trust. Skills installed from a remote source (ClawHub, Git, or a registry) are refused by `skills test` until an OS-level sandbox backend is available, because their functional tests execute skill-authored commands.

For a worked example that turns a built-in tool into a reusable operator workflow, see [using relationship memory from skills](./relationship-memory-skill-template.md).

## Install screening

Beyond the structural audit (script safety, below), remote installs are screened for content-level risk before the skill is promoted into your workspace: prompt-injection prose, embedded credential material, remote-execution patterns (such as `curl … | sh`), and encoding smuggling (malformed Unicode TAG runs, zero-width joiners, bidirectional controls). Screening is a **screening signal**, not a guarantee: static analysis cannot catch a payload constructed to evade it, and files it cannot read as text (binary, oversized, non-UTF-8, nested archives) are listed as unscanned rather than silently passed.

Screening is configured under `[skills.install_screening]`. `remote_action` (`off` | `warn` | `confirm` | `block`, default `confirm`) governs ClawHub/Git/registry installs; `local_action` (`off` | `warn`, default `warn`) governs local-path installs and never blocks your own iteration loop. Under `confirm`, a denial-impact finding (a malformed Unicode TAG run or a high-confidence embedded credential) stops the install and prints the report plus a **staged content hash**. To proceed you accept that exact content — on an interactive terminal by confirming the prompt, or non-interactively by re-running with `--accept-risk=<hash>`. The acceptance is content-bound: the re-run re-fetches and re-hashes, so if the source now serves different bytes the stale acceptance is rejected. Under `block`, a denial cannot be overridden.

Screen a source without installing it:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills screen clawhub:release-check
```

</div>

`zeroclaw skills screen` stages the source, prints the full report (including any unscanned files), and exits non-zero if any denial-impact signal is present.

## Install receipts and verification

Each successful install writes a receipt to a daemon-owned state directory (`<install root>/state/skill-receipts/`, outside the agent-writable workspace) recording the sanitized source, an immutable resolution (the Git commit SHA or downloaded-artifact SHA-256), a content tree hash, and what screening observed. Verify installed skills against their receipts:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills verify release-check
zeroclaw skills verify
```

</div>

Verification recomputes the content tree hash and reports `matches receipt`, `modified` (a local edit or an upstream content swap — reinstall to restore), or `no receipt` (a pre-provenance install). Receipts detect content swaps and accidental edits; they are **not** tamper-proof against a process running as the same user, which can rewrite both a skill and its receipt.

## Updating a skill

Reinstall over an existing skill with `--force`, which performs a journaled swap (the current skill is moved aside, the new one promoted, and the old content restored if the promotion fails):

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills install --force clawhub:release-check
```

</div>

The update is reviewed against the prior receipt. A version bump proceeds as an ordinary update; a content change that keeps the same version is flagged as a content swap and requires a content-bound override (the same `--accept-risk=<hash>` flow as screening); identical content is a no-op.

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

## See also

- [Tools overview](./overview.md)
- [Using relationship memory from skills](./relationship-memory-skill-template.md)
- [Security overview](../security/overview.md)
- [Tool receipts](../security/tool-receipts.md)
