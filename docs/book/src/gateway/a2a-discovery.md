# A2A agent discovery

This deployment can publish its agents so another deployment, or any HTTP
client, can find them and call them. A2A is the protocol for one agent to reach
another agent, the way a person reaches a bot over a chat app. This page shows
exactly what to type and exactly what comes back.

Every response on this page is real output from a running daemon. Nothing here
is illustrative.

## The whole thing in two requests

You only ever need two GET requests to discover an agent.

First, ask the deployment which agents it publishes:

```
curl http://localhost:42617/.well-known/agents-card.json
```

Second, ask one of those agents what it can do:

```
curl http://localhost:42617/a2a/agent_alpha/.well-known/agent-card.json
```

The first request gives you a list of agent URLs. The second gives you one
agent's skills and the URL you send work to. That is the entire discovery
surface. The rest of this page is just reading those two responses carefully.

## Request 1: list the agents

```
curl http://localhost:42617/.well-known/agents-card.json
```

Response:

```json
{
    "name": "ZeroClaw agents",
    "description": "Discovery catalog enumerating published A2A agents on this ZeroClaw install. Not a runnable agent; each entry below serves its own A2A card and endpoint. Skills are aggregated from the published agents, each tagged with its owning alias.",
    "supportedInterfaces": [
        {
            "url": "http://localhost:42617/.well-known/agents-card.json",
            "protocolBinding": "catalog",
            "protocolVersion": "1.0"
        },
        {
            "url": "http://localhost:42617/a2a/agent_beta",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        },
        {
            "url": "http://localhost:42617/a2a/agent_alpha",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }
    ],
    "version": "0.8.0",
    "capabilities": {
        "streaming": false,
        "pushNotifications": false,
        "extendedAgentCard": false
    },
    "defaultInputModes": ["text"],
    "defaultOutputModes": ["text"],
    "skills": [
        {
            "id": "agent_beta/github-issue-triage",
            "name": "github-issue-triage",
            "description": "Issue triage and lifecycle management agent for ZeroClaw.",
            "tags": ["github", "issues", "triage", "agent_beta"]
        },
        {
            "id": "agent_beta/github-pr-review-session",
            "name": "github-pr-review-session",
            "description": "Human-reviewer co-pilot for ZeroClaw PR reviews.",
            "tags": ["github", "pull-requests", "review", "agent_beta"]
        },
        {
            "id": "agent_alpha/zeroclaw",
            "name": "zeroclaw",
            "description": "Help users operate and interact with their ZeroClaw agent instance.",
            "tags": ["operations", "cli", "gateway", "agent_alpha"]
        },
        {
            "id": "agent_alpha/skill-creator",
            "name": "skill-creator",
            "description": "Create new skills, modify and improve existing skills, and measure skill performance.",
            "tags": ["skills", "authoring", "evaluation", "agent_alpha"]
        },
        {
            "id": "agent_alpha/changelog-generation",
            "name": "changelog-generation",
            "description": "Changelog generation skill for ZeroClaw releases.",
            "tags": ["changelog", "release", "automation", "agent_alpha"]
        }
    ]
}
```

Read it like this. `supportedInterfaces` lists URLs. The one tagged `catalog` is
this list itself, ignore it. The two tagged `JSONRPC` are the agents:
`agent_alpha` and `agent_beta`. Their URLs are where you will send work. `skills`
aggregates every published agent's skills, each `id` prefixed and `tags`-tagged
with the owning alias, so one read shows the whole install's capability surface
and who owns each piece.

## Request 2: inspect one agent

Take a URL from the list and append the card path:

```
curl http://localhost:42617/a2a/agent_alpha/.well-known/agent-card.json
```

Response:

```json
{
    "name": "agent_alpha",
    "description": "ZeroClaw agent 'agent_alpha'.",
    "supportedInterfaces": [
        {
            "url": "http://localhost:42617/a2a/agent_alpha",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }
    ],
    "version": "0.8.0",
    "capabilities": {
        "streaming": false,
        "pushNotifications": false,
        "extendedAgentCard": false
    },
    "defaultInputModes": ["text"],
    "defaultOutputModes": ["text"],
    "skills": [
        {
            "id": "zeroclaw",
            "name": "zeroclaw",
            "description": "Help users operate and interact with their ZeroClaw agent instance.",
            "tags": ["operations", "cli", "gateway"]
        },
        {
            "id": "skill-creator",
            "name": "skill-creator",
            "description": "Create new skills, modify and improve existing skills, and measure skill performance.",
            "tags": ["skills", "authoring", "evaluation"]
        },
        {
            "id": "changelog-generation",
            "name": "changelog-generation",
            "description": "Changelog generation skill for ZeroClaw releases.",
            "tags": ["changelog", "release", "automation"]
        }
    ]
}
```

Now you know three things. The agent is named `agent_alpha`. It has three skills,
`zeroclaw`, `skill-creator`, and `changelog-generation`, with plain descriptions
of what each does. And the single `JSONRPC` interface URL,
`http://localhost:42617/a2a/agent_alpha`, is the address you POST a task to.

The card `description` comes from the alias identity document when one is
configured: an AIEOS identity's bio supplies the line, falling back to a name
from that identity. When no identity is set, the card uses the neutral default
`ZeroClaw agent '<alias>'.` shown above.

## What an agent chooses to show

An agent does not have to publish every skill it has. The `agent_beta` agent in
this same deployment publishes its own selected set:

```
curl http://localhost:42617/a2a/agent_beta/.well-known/agent-card.json
```

```json
{
    "name": "agent_beta",
    "description": "ZeroClaw agent 'agent_beta'.",
    "supportedInterfaces": [
        {
            "url": "http://localhost:42617/a2a/agent_beta",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }
    ],
    "version": "0.8.0",
    "capabilities": {
        "streaming": false,
        "pushNotifications": false,
        "extendedAgentCard": false
    },
    "defaultInputModes": ["text"],
    "defaultOutputModes": ["text"],
    "skills": [
        {
            "id": "github-issue-triage",
            "name": "github-issue-triage",
            "description": "Issue triage and lifecycle management agent for ZeroClaw."
        },
        {
            "id": "github-pr-review-session",
            "name": "github-pr-review-session",
            "description": "Human-reviewer co-pilot for ZeroClaw PR reviews."
        }
    ]
}
```

The deployment chose which agents to publish and which skills each one exposes.
That is the whole point of publishing: you decide per agent which skills the
outside world can see.

## Sending a task

Once you have an agent's interface URL and a skill, you send work as a JSON-RPC
`message/send` POST to that URL:

```
curl -X POST http://localhost:42617/a2a/agent_alpha \
  -H 'Content-Type: application/json' \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "message/send",
    "params": {
      "message": {
        "role": "user",
        "parts": [{ "kind": "text", "text": "Reply with PONG" }]
      }
    }
  }'
```

The agent runs the turn and answers with a completed task. The reply is the text
part inside the task's artifact:

```
{
    "id": 1,
    "jsonrpc": "2.0",
    "result": {
        "artifacts": [
            {
                "artifactId": "5346ae32-1b63-40c0-9aaa-345d815c792e",
                "parts": [
                    {
                        "kind": "text",
                        "text": "PONG"
                    }
                ]
            }
        ],
        "contextId": "a2a_agent_alpha_06cb22f5-12bf-4b26-9ebc-9c063ab520a4",
        "id": "0ef19fcb-b5e4-4c26-afce-d80451c8861e",
        "kind": "task",
        "status": {
            "state": "completed"
        }
    }
}
```

The interface URL is the same for discovery and for tasks; only the request
changes. The endpoint accepts only `message/send`; any other `method` returns a
JSON-RPC `-32601`, an empty message returns `-32602`, and a body that is not
JSON-RPC returns HTTP `400`.

## Exposure and the one sharp edge

The task endpoint shares the exact posture of the cards: it answers only when
`[a2a.server] enabled` is set and the alias is enabled and published. A task POST
to an unpublished or unknown alias returns `404`, the same as its card.

One sharp edge to know about: the interface URL answers a bare GET with the web
dashboard, not an agent, because the gateway falls back to serving the dashboard
for any path it does not recognize:

```
curl -i http://localhost:42617/a2a/translator
HTTP/1.1 200 OK
content-type: text/html
```

Discovery (the `.well-known` paths) and the `message/send` POST are the supported
surface. A bare GET on the interface URL is not part of the protocol; read the
card at the `.well-known` path instead.

## Where the agents serve from

The cards are served by the web gateway, on the same address and port as
everything else. If your gateway is on `localhost:42617`, that is where the
catalog and every agent card live. You do not run a second server and you do not
open a second port.

If you put this deployment behind a reverse proxy or a public hostname, the URLs
inside the cards need to match the address clients actually reach. The published
URL is resolved in this order: an explicit public base URL if you set one, then
an A2A-specific host and port override if you set those, then the gateway's own
address. The override exists for the proxy case; if you are not behind a proxy
you never touch it and the cards advertise the gateway address directly.

## Turning it on

Discovery is off until you turn it on, and it is off in three independent ways
so nothing leaks by accident:

- The A2A server is disabled for the whole deployment by default.
- Each agent is unpublished by default, even with the server on.
- A published agent exposes only the skills you name, nothing more.

You enable the server once, mark the specific agents you want reachable as
published, and list the skills each one exposes. An agent that is disabled, or
not published, does not appear in the catalog and its card path returns `404`. An
unknown agent name returns `404` as well.

A named skill appears on the card only when it resolves to a real skill the agent
actually carries: it must live in one of the agent's skill bundles and its
`SKILL.md` must have valid YAML frontmatter. A name that does not resolve, or a
skill in a bundle the agent does not declare, is dropped silently rather than
advertised.

### What publishing actually exposes

Read this before you publish. Once the server is enabled and an alias is
published, `POST /a2a/{alias}` runs a full agent turn for that alias: it invokes
the agent through the same path the chat surfaces use, with the agent's entire
configured toolset (shell, file, browser, and whatever else that alias carries).

That endpoint is not behind the gateway's bearer/pairing auth. Authentication in
this gateway is enforced per handler, and the A2A task handler does not gate on a
token, by design: cross-deployment interop only works if a peer can reach the
endpoint without sharing your login. The practical consequence is direct:
**anyone who can reach the gateway listener can invoke a published agent, tools
and all, with no credential**, while neighboring endpoints like `/api/config`
require a bearer token.

This is a deliberate trade for frictionless agent-to-agent calls, not an
oversight, but it means publishing is a real exposure decision. Before you flip
the switches:

- Scope the bind posture. Bind the gateway to a private interface, or sit it
  behind a reverse proxy that enforces auth, rather than exposing the listener
  directly to an untrusted network.
- Publish only aliases whose full toolset you are willing to have invoked by any
  reachable caller, and narrow `exposed_skills` to the minimum that interop
  needs.
- Treat a published alias as a remotely-invokable execution surface when you
  decide which tools and skill bundles that alias carries.

## How several deployments connect

Discovery composes across any number of deployments. Each deployment publishes
its own catalog at its own address. A client that knows several deployment
addresses fetches each catalog, reads the agents, and now holds a combined map of
every reachable agent across all of them. There is no registry and no central
server: the client is the only thing that needs to know the addresses, and it
talks to each deployment directly.

A worked picture. You run a personal deployment. Your team runs a shared one. A
data team runs a third. Your client fetches all three catalogs:

```
curl http://personal.example:42617/.well-known/agents-card.json
curl http://team.example:42617/.well-known/agents-card.json
curl http://data.example:42617/.well-known/agents-card.json
```

Each returns its own agent list. Your client now sees, say, a `notes` agent at
personal, a `deploy` agent at team, and a `query` agent at data. To use any of
them it fetches that agent's card and sends a task to that agent's URL, exactly
as shown above. Nothing changes per deployment; it is the same two reads and one
POST, pointed at a different host.

## Use cases

A few concrete reasons to wire deployments together.

A research deployment hands literature search to a specialist data deployment.
The research agent discovers the data deployment's `search` agent, sends it a
query as a task, and folds the result into its own work. The research side never
holds the data side's credentials or indexes; it only knows the agent URL.

An on-call deployment fans an incident out to team-owned deployments. It
discovers a `triage` agent in each team's deployment and sends each one the same
incident as a task, collecting their answers. Each team controls what their
triage agent exposes; the on-call side just reads cards and sends tasks.

A personal deployment calls a company deployment's vetted agents without sharing
logins. You discover the company's `invoice` agent, send it a draft request, and
get a result back. The company decides which agents and skills are published; you
never get a seat inside their deployment, only the agent endpoint.

## A2A is not MCP

These solve different problems and they compose. MCP connects one agent to its
tools and context: it answers what a single agent can call. A2A connects an
agent to other agents as peers: it answers which other agents it can hand work
to. An agent you reach over A2A may use MCP tools internally to do the job, and
you neither see nor care; the card shows skills, not the tools behind them. Use
MCP to give an agent capabilities, use A2A to let agents delegate to each other.
