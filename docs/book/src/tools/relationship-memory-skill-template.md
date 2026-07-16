# Using Relationship Memory From Skills

Use this template when you want a skill to tell an agent when and how to capture workflow, capability, and steward-role relationships with the `knowledge` tool. It keeps the graph as an operator-facing workflow, not only an internal capability.

Create a bundle for relationship-memory skills, then scaffold the skill:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills bundle add relationship-memory
zeroclaw skills add relationship-memory-capture \
  --bundle relationship-memory \
  --description "Capture and query durable workflow relationships with the knowledge graph" \
  --edit
```

</div>

The `skills add` command opens `SKILL.md` in your editor. Replace the generated file contents with this template:

```markdown
---
name: relationship-memory-capture
description: Capture and query durable workflow relationships with the knowledge graph
version: 0.1.0
author: zeroclaw_operator
tags: [knowledge, memory, relationships]
---

# Relationship memory capture

Use this skill when the operator asks to remember or inspect how workflows, skills, capabilities, decisions, lessons, or steward roles connect to each other.

Before writing graph memory:

1. Confirm the deployment has `[knowledge] enabled = true`.
2. Ask what relationship should be remembered and why it should be durable.
3. Ask whether the entry may include private workspace, account, client, or contact context.
4. Refuse to store secrets, tokens, personal email addresses, phone numbers, private URLs, account IDs, session IDs, or credential-like values.

Capture durable things as graph nodes with the `knowledge` tool:

- Use `pattern` for reusable workflows, practices, or skill-shaped procedures.
- Use `technology` for tools, protocols, providers, or capability surfaces.
- Use `expert` for steward roles, maintainer roles, or agent roles. Prefer role labels over real names.
- Use `decision` for design choices, policy calls, or accepted direction.
- Use `lesson` for something learned from an incident, review, or run.

Relate nodes after capture:

- Use `uses` from a workflow or skill to a capability it depends on.
- Use `authored_by` from a workflow or skill to the role that maintains it.
- Use `applies_to` from a lesson, decision, or pattern to the project or capability it affects.

When the operator asks what is connected to a workflow, capability, decision, lesson, or steward role, call `graph_neighbors`.

Keep entries short and factual. If a relationship is uncertain, capture the uncertainty in the content field or ask before writing it.
```

To make the skill available to an agent, attach the `relationship-memory` bundle to that agent. For example, append the bundle alias to `agents.<alias>.skill_bundles` in config, or use the dashboard config editor.

## Capture a workflow dependency

These examples show model-facing `knowledge` tool arguments. They are not CLI subcommands.

Capture the workflow:

```json
{
  "action": "capture",
  "node_type": "pattern",
  "title": "Release check workflow",
  "content": "Checks version notes, validation evidence, changelog coverage, and rollback notes before release sign-off.",
  "tags": ["skill", "release"]
}
```

Capture the capability it uses:

```json
{
  "action": "capture",
  "node_type": "technology",
  "title": "Docs validation capability",
  "content": "Runs docs quality and changed-link validation for mdBook source changes.",
  "tags": ["capability", "docs"]
}
```

Relate the workflow to the capability:

```json
{
  "action": "relate",
  "from_id": "<release-check-workflow-node-id>",
  "to_id": "<docs-validation-capability-node-id>",
  "relation": "uses"
}
```

Capture the steward role and relate it to the workflow:

```json
{
  "action": "capture",
  "node_type": "expert",
  "title": "release-steward role",
  "content": "Role that maintains release readiness checks and decides when evidence is sufficient.",
  "tags": ["role", "release"]
}
```

```json
{
  "action": "relate",
  "from_id": "<release-check-workflow-node-id>",
  "to_id": "<release-steward-role-node-id>",
  "relation": "authored_by"
}
```

Then inspect the workflow:

```json
{
  "action": "graph_neighbors",
  "node_id": "<release-check-workflow-node-id>",
  "limit": 10
}
```

## Project relationships

The same skill pattern can cover project or client relationships, but keep that as an explicit extension. Add `client`, `contact`, and `interaction` node guidance only when the operator wants project relationship tracking and has a privacy policy for that data. The [relationship memory](./relationship-memory.md) page shows the `client_network` and `interaction_log` actions for that shape.

## Validate the installed skill

Audit the skill directory after saving it. With the default install root, the scaffolded bundle skill lives under `~/.zeroclaw/shared/skills/<bundle>/<skill>/`:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw skills audit ~/.zeroclaw/shared/skills/relationship-memory/relationship-memory-capture
```

</div>

If your install root is not `~/.zeroclaw`, audit the path printed by `zeroclaw skills add`.

`skills audit` checks the skill package shape. It does not prove that your deployment has the `knowledge` tool enabled or that the graph contains useful data. For that, run a small manual session with placeholder data and confirm the agent can call `capture`, `relate`, and `graph_neighbors` for a workflow/capability pair. If you extend the template for projects, also confirm `client_network` and `interaction_log` return the expected placeholder entries.

## See also

- [Relationship memory](./relationship-memory.md)
- [Skills](./skills.md)
- [Privacy & PII discipline](../contributing/privacy.md)
