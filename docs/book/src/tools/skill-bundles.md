# Writing a Skill Bundle

A skill bundle is the one plugin kind that ships no WebAssembly at all. It is
a directory of markdown skills, packaged and distributed through the plugin
machinery: same manifest, same discovery, same signature policy, same
`zeroclaw plugin install`. Use it when the capability you are adding is
instructions, prompts, and workflows rather than code, and you want plugin
distribution semantics (signing, registry install, versioning) instead of
loose files in a skills directory.

This guide is checked against the validation path in
`crates/zeroclaw-plugins/src/host.rs` (`validate_skill_bundle`,
`validate_skill_md_frontmatter`) and the loader in
`crates/zeroclaw-runtime/src/skills/mod.rs`.

For what a skill itself is and how agents use them, read
[Skills](../tools/skills.md) first. This page covers only the bundle
packaging.

## Layout

A skill-only plugin omits `wasm_path` and carries a `skills/` directory in
[agentskills.io](https://agentskills.io) format:

```text
my-toolkit/
  manifest.toml           # capabilities = skill only, no wasm_path
  README.md               # optional bundle-level overview
  skills/
    design-review/
      SKILL.md
      scripts/            # optional
      references/         # optional
    code-review/
      SKILL.md
    data-analysis/
      SKILL.md
      references/
```

## Validation: what discovery enforces

The host validates the bundle shape at discovery and install, and rejects the
whole plugin on the first failure (`validate_skill_bundle` in `host.rs`).
The exact rules:

1. `skills/` must exist and be a directory.
2. It must contain at least one subdirectory. An empty `skills/` is an
   invalid manifest, not an empty bundle.
3. Every subdirectory must contain a `SKILL.md`.
4. Every `SKILL.md` must open with YAML frontmatter (a `---` fence on line
   one, terminated by a closing `---`), and that frontmatter must declare
   non-empty `name` and `description` keys.

The frontmatter check runs at discovery time on purpose: a bundle whose
skills omit `name` or `description` fails when the plugin loads, not when an
agent first invokes the skill mid-conversation.

A valid skill header:

```markdown
---
name: design-review
description: Structured design review workflow for architecture proposals.
---

# Design Review

...instructions...
```

## Namespacing

Loaded bundle skills register under plugin-qualified IDs:
`plugin:<plugin-name>/<skill-name>`, e.g. `plugin:my-toolkit/design-review`
(`namespace_plugin_skill` in `skills/mod.rs`). Each skill also receives a
`plugin:<plugin-name>` tag. This prevents collisions with user-authored
skills and between bundles: two bundles can both ship a `code-review` skill
and coexist.

The namespacing interacts with skill precedence: in the agent's
effective-skill resolution, same-name skills from different sources are
deduplicated by precedence and the losers are recorded as shadowed. The
plugin qualifier keeps your bundle out of that fight entirely unless another
copy of the same bundle name is in play.

## Scripts

A skill may carry a `scripts/` directory. Whether script-bearing skills load
is governed by the operator's `skills.allow_scripts` setting, which the
plugin-skill loader passes through unchanged (`discover_plugin_skills` in
`skills/mod.rs`): a bundle skill with scripts is subject to exactly the same
audit-and-drop rules as a workspace skill. Do not assume your scripts run
just because the bundle installed.

## Manifest

{{#include ../_snippets/plugin-manifest-fields.md}}

For a skill bundle: `capabilities` containing exactly `skill`, no
`wasm_path`, and typically no `permissions` at all; the bundle is data, and
the permission set gates host functions that markdown never calls.

A mixed-capability plugin (say `tool` + `skill`) is legal: it must then carry
a valid `wasm_path` for the tool world *and* a valid `skills/` bundle, and
both validations run.

## Install and verify

{{#include ../_snippets/plugin-install-layout.md}}

After discovery, the skills appear namespaced in the skills surfaces (the
skills list, the dashboard) as `plugin:<your-bundle>/<skill>`. Ask the agent
to use one to confirm end to end.

## Next

- [Distributing plugins](../plugins/distributing-plugins.md): a skill bundle
  is the simplest thing to publish, and the signing story is identical to
  WASM plugins.
