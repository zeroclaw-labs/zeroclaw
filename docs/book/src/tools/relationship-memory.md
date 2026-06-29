# Relationship Memory

Relationship memory is the opt-in graph side of the `knowledge` tool. Use it when an agent needs to remember how things connect, not only which text snippet matches a query.

This is separate from ordinary long-term memory. The `knowledge` tool is backed by the `zeroclaw-memory` knowledge graph, while ordinary memory remains the `Memory` backend surfaced through `memory_*` tools.

Ordinary long-term memory answers questions like "what do we know about this topic?" Structured relationship memory answers questions like "which nodes are connected to this thing?", "who manages this client?", or "which skill workflows use this capability?"

## Enable the knowledge graph

The graph tool is disabled by default. Enable it in config before expecting an agent to call `knowledge`:

```toml
[knowledge]
enabled = true
```

Relationship capture is explicit today. Agents store graph entries through `knowledge` actions such as `capture` and `relate`; enabling the tool does not turn on automatic ingestion. Relationship memory can hold sensitive operational or business context, so operators should choose what gets stored.

## Concepts

The graph stores nodes and directed edges.

Node types:

- `pattern`: reusable workflow, practice, or skill-shaped procedure
- `decision`: design choice, policy call, or accepted direction
- `lesson`: something learned from an incident, review, or run
- `expert`: an owner, steward, maintainer role, or agent role
- `technology`: provider, tool, protocol, or capability surface
- `client`: account, project, workspace, or external group being tracked
- `contact`: contact point for a client or project
- `interaction`: meeting, request, review, incident, or other dated touchpoint

Relations:

- `uses`
- `replaces`
- `extends`
- `authored_by`
- `applies_to`
- `manages_client`
- `contact_of`
- `interacted_with`

Direction matters. For example, a contact points to the client with `contact_of`, and a client points to an interaction with `interacted_with`.

## Tool actions

The `knowledge` tool supports general memory actions such as `capture`, `search`, `relate`, `suggest`, `expert_find`, `lessons_extract`, and `graph_stats`.

The relationship-oriented actions are:

| Action | Required field | Use |
|---|---|---|
| `graph_neighbors` | `node_id` | Return inbound and outbound graph edges for any node. |
| `client_network` | `client_id` | Return contacts, managers, and recent interactions for a `client` node. |
| `interaction_log` | `client_id` | Return recent interaction nodes connected to a `client` node. |

These are model-facing tool actions, not CLI subcommands. The JSON examples below show the arguments an agent can pass when a skill or instruction asks it to capture or inspect graph relationships.

## Client relationship example

Create the client node. `capture` returns `node_id`; use that value as `<client-node-id>` below:

```json
{
  "action": "capture",
  "node_type": "client",
  "title": "Example workspace",
  "content": "A neutral placeholder workspace used to demonstrate relationship memory.",
  "tags": ["example", "workspace"]
}
```

Create a contact node, then relate it to the client:

```json
{
  "action": "capture",
  "node_type": "contact",
  "title": "Project contact",
  "content": "Primary contact role for the example workspace. Do not store personal contact details unless policy allows it.",
  "tags": ["example", "contact"]
}
```

```json
{
  "action": "relate",
  "from_id": "<contact-node-id>",
  "to_id": "<client-node-id>",
  "relation": "contact_of"
}
```

Create an interaction and attach it to the client:

```json
{
  "action": "capture",
  "node_type": "interaction",
  "title": "Scope review",
  "content": "Reviewed the next implementation slice and agreed to keep UI helpers out of scope.",
  "tags": ["example", "review"]
}
```

```json
{
  "action": "relate",
  "from_id": "<client-node-id>",
  "to_id": "<interaction-node-id>",
  "relation": "interacted_with"
}
```

Ask for the current network:

```json
{
  "action": "client_network",
  "client_id": "<client-node-id>"
}
```

Ask for recent interactions:

```json
{
  "action": "interaction_log",
  "client_id": "<client-node-id>",
  "limit": 10
}
```

## Skill capability example

Relationship memory is not only for client-style tracking. A skill can guide the agent to build a capability graph from installed skill workflows.

Example `SKILL.md`:

```markdown
---
name: capability-map
description: Capture and query the relationship between local skills, capabilities, and steward roles
version: 0.1.0
author: zeroclaw_operator
tags: [knowledge, skills, capability-map]
---

# Capability map

Use the `knowledge` tool when the operator asks which skill, capability, or steward role is connected to a workflow.

Capture each reusable skill workflow as a `pattern` node. Capture reusable tool or platform surfaces as `technology` nodes. Capture maintainer or agent stewardship roles as `expert` nodes, using role names rather than personal details.

Use `uses` from a skill workflow to a capability it relies on. Use `authored_by` from a skill workflow to the role that maintains it. Use `graph_neighbors` when the operator asks what a skill depends on or what depends on a capability.

Do not store secrets, personal contact details, private URLs, or account identifiers. Summarize sensitive context outside the graph or ask before storing it.
```

The agent could capture and relate a skill workflow like this:

```json
{
  "action": "capture",
  "node_type": "pattern",
  "title": "Release check skill",
  "content": "Reviews changelog, version tags, validation evidence, and rollback notes before release sign-off.",
  "tags": ["skill", "release"]
}
```

```json
{
  "action": "capture",
  "node_type": "technology",
  "title": "Docs translation workflow",
  "content": "Uses the mdBook translation pipeline for source docs and translation cache validation.",
  "tags": ["capability", "docs"]
}
```

```json
{
  "action": "relate",
  "from_id": "<release-check-skill-node-id>",
  "to_id": "<docs-translation-capability-node-id>",
  "relation": "uses"
}
```

Then the operator can ask "what does the release check skill depend on?" and the agent can call:

```json
{
  "action": "graph_neighbors",
  "node_id": "<release-check-skill-node-id>",
  "limit": 10
}
```

## Privacy rules

Relationship memory is durable. Treat it like any other public or shared knowledge store:

- Use neutral placeholders in examples and tests.
- Do not store secrets, tokens, personal email addresses, private URLs, account IDs, or session IDs.
- Avoid storing personal contact details unless the operator's deployment policy explicitly allows it.
- Prefer role labels such as `ZeroClawMaintainer`, `release-steward`, or `project-contact` over real names.
- Keep autonomous ingestion off unless the data source, retention policy, and review path are clear.

## See also

- [Tools overview](./overview.md)
- [Skills](./skills.md)
- [Privacy & PII discipline](../contributing/privacy.md)
