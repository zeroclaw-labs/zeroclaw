# Filesystem components

The relational half of an agent points at config; the on-disk half lives under
the install root. The layout is organized by **scope**, not one flat tree:
instance-wide state, cross-agent shared resources, and per-agent private data
each get their own top-level directory.

```text
<install>/
├── data/                         — instance-wide state (not per-agent)
│   ├── sessions/                 — chat session stores
│   │   └── sessions.db, acp-sessions.db
│   ├── state/                    — runtime state
│   │   └── costs.jsonl, runtime-trace.jsonl
│   ├── devices.db                — paired-device metadata
│   └── memory/                   — shared instance memory
│       └── brain.db, audit.db, response_cache.db,
│           MEMORY_SNAPSHOT.md, archive/
├── shared/                       — resources agents draw on in common
│   └── skills/<bundle>/          — skill bundles
│       └── SKILL.md, scripts/, references/, assets/
└── agents/                       — per-agent private data
    └── <alias>/
        └── workspace/            — the agent's jailed filesystem sandbox
            └── memory/
                └── brain.db
```

The three roots map to three scopes:

- **`data/`** holds state that belongs to the whole install, not to any one
  agent: chat `sessions/`, runtime `state/` (cost tracking and the like), the
  paired-device metadata in `devices.db`, and the shared instance memory under
  `data/memory/`. Valid gateway bearer tokens remain config state; `devices.db`
  makes paired devices visible and manageable.
- **`shared/`** holds resources agents draw on in common, notably skill bundles
  under `shared/skills/<bundle>/`.
- **`agents/<alias>/`** holds everything private to one agent. By default an
  agent's workspace is `<install>/agents/<alias>/workspace/`, and everything the
  agent reads or writes stays inside it. The agent's identity source is resolved
  relative to this workspace. Agents are **jailed** to their own workspace
  unless you explicitly grant cross-agent access.

## Workspace

The workspace is the agent's filesystem sandbox. The fields below are generated
from the schema:

{{#config-fields agents.workspace}}

Two things worth calling out:

- **`access`** is an inbound allowlist for cross-agent filesystem sharing. Empty
  means jailed (own workspace only); an entry grants a named sibling agent a
  read or write mode into this agent's workspace.
- **`unrestricted_filesystem`** is the escape hatch: when `true`, the agent can
  touch anything the host filesystem permits. It is off by default and flipping
  it on is auditable.

## Shared resources

`<install>/shared/` holds resources agents draw on in common. An agent reaches
it only if you opt that agent in:

```toml
[agents.researcher]
can_use_shared_workspace = true
```

The flag is **deny-by-default**. With it off, the only reach an agent has into
`shared/` is the code-enforced read-only wire to its own skill bundles under
`shared/skills/<bundle>/`. With it on, `<install>/shared/` is added to the
agent's read-only allowlist. The environment form follows the usual convention:
`ZEROCLAW_agents__<alias>__can_use_shared_workspace=true`.

The grant is **read-only by design**, not by omission. Allowlist matching is a
path-prefix test and explicit allowed roots are consulted before
`forbidden_paths`, so a writable root over the whole of `shared/` would shadow
the narrower `shared/skills/` wire and let one agent overwrite skills that other
agents execute. Write access to `shared/`, if you ever need it, needs a narrower
surface than this flag.

The location derives from the config file's own directory
(`config_path.parent()/shared`), not from `data_dir`. An install that points
`data_dir` elsewhere still shares the directory beside `config.toml`.

Three limits are worth knowing before you turn it on:

- **Reads are exact-path.** The file tools honor the grant, so an agent can read
  a file under `shared/` when it already knows the path.
- **Glob discovery stays workspace-only.** Glob-style path search deliberately
  ignores the read-only tier, so files under `shared/` are readable but are not
  enumerated by that search. The agent will not discover them by listing; give it
  the path.
- **Shell access is bounded separately.** Under an active OS sandbox (Landlock
  or Seatbelt) shell commands touching `shared/` are denied outright, because the
  sandbox is built from the workspace directory alone and does not yet receive
  the allowlist tiers. That is fail-closed, an availability gap rather than a
  widening. With the sandbox disabled or pass-through, a shell **redirect** into
  a read-only root is refused: a redirect target must belong to a write tier,
  never merely a readable one. For a non-redirect argument the static scan cannot
  tell a read from a write, so the OS sandbox remains the complete write boundary
  for shell.

## Memory

Each agent keeps its own memory store under its workspace
(`agents/<alias>/workspace/memory/`), separate from the shared instance memory
in `data/memory/`. The backend is selected per agent:

{{#config-fields agents.memory}}

The backend defaults to SQLite for a new agent, and once the agent has written
on-disk data the value is locked, so you cannot silently swap a backend out from
under existing memory. Cross-agent memory sharing is opt-in through the
workspace `read_memory_from` allowlist. For the memory model itself, see
[Runtime internals](./internals.md). For a cross-system state map, see
[Runtime state and persistence](../architecture/runtime-state-and-persistence.md).

## Identity

An agent's identity (its personality) is sourced per agent:

{{#config-fields agents.identity}}

The `format` selects how the identity is loaded. The default reads the
project's personality files; the alternative loads an AIEOS JSON definition,
either from a path relative to the workspace or inline.
