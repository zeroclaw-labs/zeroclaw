# Anatomy of an agent

An agent is configured as a single `[agents.<alias>]` block. Every field is
either a reference to something configured elsewhere or a per-agent override.
The table below is generated from the config schema, so it always matches the
running build. Click a field to expand it; click again to see how to set it.

{{#config-fields agents}}

## The reference axes

Most of an agent's config is dotted aliases pointing at other sections. The
agent owns none of these, it points, and the same target can be shared by many
agents.

- **`model_provider`** points at a `[providers.models.<type>.<alias>]` entry.
  The companion `tts_provider`, `transcription_provider`, and
  `classifier_provider` point at their own provider entries. All of these live
  in [Model Providers](../providers/overview.md).
- **`risk_profile`** and **`runtime_profile`** name a
  `[risk_profiles.<alias>]` and `[runtime_profiles.<alias>]`. The risk profile
  sets the autonomy and sandbox posture; the runtime profile sets operational
  tuning (tool-iteration caps, budgets, timeouts, context limits). Both are
  explained in [Security & Autonomy](../security/autonomy.md).
- **`channels`** lists the channel instances the agent answers on, each a dotted
  `<type>.<alias>` into `[channels]`. See [Channels](../channels/overview.md).
  When two agents share a channel, a [peer group](../channels/peer-groups.md)
  decides whether they can address each other.
- **`skill_bundles`**, **`knowledge_bundles`**, and **`mcp_bundles`** attach
  reusable groups of skills, knowledge, and MCP servers by alias. See
  [Tools](../tools/overview.md).
- **`cron_jobs`** binds named scheduled jobs to the agent.

## The per-agent overrides

A handful of fields are not references but per-agent settings that override a
global default: the `workspace`, `memory`, and `identity` blocks. Those are the
on-disk side of the join and are covered in
[Filesystem components](./filesystem.md).

## Validation

`Config::validate()` fails loud at startup if `model_provider` does not resolve
to a configured provider entry, or if `risk_profile` does not resolve to a
configured risk profile. A bad reference is caught before the agent runs, not
silently ignored.
