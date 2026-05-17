# Design: Skill `enabled` Field

**Date:** 2026-05-09
**Status:** Approved

## Summary

Add an `enabled` boolean field to SKILL.md frontmatter and SKILL.toml manifests. When `enabled: false`, the skill is loaded into memory but excluded from the agent's system prompt and tool registry. Disabled skills remain visible in `zeroclaw skills list` with a `[disabled]` indicator.

## Motivation

Currently there is no way to temporarily disable a skill without deleting or moving its directory. Setting `enabled: false` in the skill file provides a lightweight toggle that survives across restarts.

## Data Model

### `Skill` struct (`zeroclaw-runtime/src/skills/mod.rs`)

Add field:
```rust
#[serde(default = "default_true")]
pub enabled: bool,
```

### `SkillMeta` struct (TOML format)

Add field:
```rust
#[serde(default = "default_true")]
enabled: bool,
```

### `SkillMarkdownMeta` struct (MD format)

Add field:
```rust
enabled: Option<bool>,
```

## Parsing

### SKILL.md frontmatter

`parse_simple_frontmatter` gains a new match arm:

```
"enabled" => parse value using same logic as parse_open_skills_enabled
             accepts: true/false/1/0/yes/no/on/off (case-insensitive)
             invalid values → default true, no error
```

### SKILL.toml

Handled automatically by serde with `default = "default_true"`.

## Loading Behavior

`load_skills_from_directory` and `load_open_skills_from_directory` load **all** skills regardless of `enabled`. No filtering at load time.

## Runtime Filtering

Two sites filter out disabled skills:

- `skills_to_prompt` — skips skills where `!skill.enabled`
- `skills_to_tools` — skips skills where `!skill.enabled`

## `skills list` Output

The CLI handler for `zeroclaw skills list` appends `[disabled]` to the name/row of any skill where `enabled == false`.

## Defaults

- `enabled` defaults to `true` in all formats.
- Open skills (`load_open_skill_md`) hard-code `enabled: true`; the field is not meaningful for community skills.

## Example SKILL.md

```markdown
---
name: deep-web-research
description: Multi-intent concurrent search and deep web crawling.
enabled: false
metadata:
  version: "v0.0.1"
---

Skill instructions here...
```

## Files Changed

- `crates/zeroclaw-runtime/src/skills/mod.rs` — structs, parser, load functions, prompt/tools filters
- `src/skills/mod.rs` — CLI `skills list` handler display
