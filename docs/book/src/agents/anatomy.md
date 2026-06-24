# Anatomy of an agent

An agent is configured as a single `[agents.<alias>]` block. Every field is
either a reference to something configured elsewhere or a per-agent override.
The table below is generated from the config schema, so it always matches the
running build. Click a field to expand it; click again to see how to set it.

{{#config-fields agents}}

## Where the references point

Most of an agent's config is dotted aliases pointing at things configured in
their own sections. The agent owns none of them, it points, and the same target
can be shared by many agents. The field table above is the authoritative list;
here is where each kind of reference leads:

- **Providers** ([Model Providers](../providers/overview.md)): the agent's
  chat model and its companion text-to-speech, transcription, and classifier
  providers each name a `[providers.models.<type>.<alias>]` entry.
- **Profiles** ([Security & Autonomy](../security/autonomy.md)): the risk
  profile sets the autonomy and sandbox posture; the runtime profile sets
  operational tuning (tool-iteration caps, budgets, timeouts, context limits).
- **Channels** ([Channels](../channels/overview.md)): the messaging surfaces
  the agent answers on. When two agents share a channel, a
  [peer group](../channels/peer-groups.md) decides whether they can address each
  other.
- **Bundles** ([Tools](../tools/overview.md)): reusable groups of skills,
  knowledge, and MCP servers attached by alias.
- **Cron**: named scheduled jobs bound to the agent.

## The per-agent overrides

Some of an agent's config is not a reference but a per-agent block that
overrides a global default: the workspace, memory, and identity. Those are the
on-disk side of the join, covered in
[Filesystem components](./filesystem.md).

## Validation

`Config::validate()` fails loud at startup if `model_provider` does not resolve
to a configured provider entry, or if `risk_profile` does not resolve to a
configured risk profile. A bad reference is caught before the agent runs, not
silently ignored.
