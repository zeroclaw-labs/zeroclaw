# Unified Wiki Remediation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify the scrapyard wiki and zeroclaw project auto-memory into a single knowledge system where the wiki is the primary store and auto-memory serves as a lightweight routing cache.

**Architecture:** The scrapyard wiki (`~/github_projects/scrapyard-wiki/`) already has mature structure (page types, cross-references, ingest/query/lint skills, log, index). The zeroclaw project auto-memory (`~/.claude/projects/.../memory/`) is a flat collection of markdown files with a MEMORY.md index. The fix: modify the global and project CLAUDE.md files to establish clear routing rules, migrate existing auto-memories to the wiki where appropriate, and adjust the auto-memory instructions to point at the wiki instead of duplicating knowledge.

**Tech Stack:** Markdown (CLAUDE.md files), scrapyard wiki skills

---

## Root Cause

Three systems compete for the same knowledge without routing logic:

| System | Trigger | Scope | Problem |
|--------|---------|-------|---------|
| Auto-memory (`~/.claude/projects/.../memory/`) | Automatic — built into system prompt, fires every conversation | Project-scoped | Captures everything, including cluster-wide knowledge that belongs in the wiki |
| Global CLAUDE.md wiki instruction | Manual — 4 lines in global config, easy to forget mid-task | Cluster-wide | Only fires when I actively remember; gets bypassed during deep debugging sessions |
| Scrapyard wiki | Manual — requires explicit `/wiki-ingest` or manual page creation | Cluster-wide | Never receives project-derived knowledge because auto-memory intercepts it first |

**The fundamental conflict:** Auto-memory's system prompt instructions say "save immediately when you learn something." The wiki instruction says "ingest after significant work." Auto-memory fires first because it's always active. By the time a session ends, the knowledge is in auto-memory and the wiki ingest never happens — the finding feels "already saved."

---

## Task 1: Update global ~/.claude/CLAUDE.md

**Files:**
- Modify: `~/.claude/CLAUDE.md`

The global CLAUDE.md needs to:
1. Establish the wiki as THE primary knowledge store (not just for "cluster-related" tasks)
2. Define clear routing rules for what goes where
3. Reference the wiki from the auto-memory context so they work together

- [ ] **Step 1: Replace the global CLAUDE.md content**

```markdown
# Global Context

## Knowledge Base

The scrapyard wiki (`~/github_projects/scrapyard-wiki/`) is the primary persistent knowledge store across all projects. Auto-memory is a lightweight session cache — the wiki is the source of truth.

### When to use the wiki

After any work session that produces knowledge worth keeping:
- Infrastructure changes (deployments, config, networking)
- Incident findings and RCAs
- Architectural decisions and rationale
- Service configurations and patterns
- Cross-project operational knowledge (deployment strategies, tool quirks, provider behaviors)

Use `/wiki-ingest` to process session findings into wiki pages. Read `~/github_projects/scrapyard-wiki/index.md` to find relevant pages before making changes.

### When to use auto-memory only

Keep these in project auto-memory (`~/.claude/projects/.../memory/`) — they are interaction-specific and don't belong in the wiki:
- Feedback on how Dan wants to work (coding preferences, communication style)
- Ephemeral project state (what PR is in flight, what branch we're on)
- References to external systems (where to find things outside the wiki)

### Routing rule

When saving a memory, ask: "Would this be useful to someone working on a different project in this cluster?" If yes → wiki. If it's about how Dan and I interact → auto-memory.

### Session end

Before ending a significant work session, check if any findings should be ingested into the wiki. This is especially important after: debugging sessions, RCAs, deployment changes, new service configurations, and migration work. A 30-second `/wiki-ingest` at session end compounds into a comprehensive knowledge base over time.
```

- [ ] **Step 2: Verify**

Read the file back to confirm formatting.

- [ ] **Step 3: Commit**

This file isn't in a git repo, so no commit needed. Just save.

---

## Task 2: Add wiki routing section to zeroclaw/CLAUDE.md

**Files:**
- Modify: `~/github_projects/zeroclaw/CLAUDE.md`

The project CLAUDE.md is long (500+ lines of engineering protocol). Add a small section that connects zeroclaw work to the scrapyard wiki, placed near the top so it's visible.

- [ ] **Step 1: Add wiki integration section after the Project Snapshot (section 1)**

Insert after section 1 and before section 2:

```markdown
## 1.1) Knowledge Routing

ZeroClaw development produces two kinds of knowledge:

- **Code knowledge** (architecture, patterns, conventions): stays in this repo — CLAUDE.md, code comments, docs/
- **Operational knowledge** (deployment patterns, infrastructure interactions, incident findings): goes to the scrapyard wiki at `~/github_projects/scrapyard-wiki/`

After work that involves K8s manifests, deployment strategies, provider configurations, networking, or incident debugging, ingest operational findings into the scrapyard wiki using `/wiki-ingest`. The wiki is shared across all cluster projects — knowledge filed there compounds.

ZeroClaw-specific entries in the scrapyard wiki:
- `wiki/services/zeroclaw` — deployment topology, configmaps, agent architecture
- `wiki/services/signal-cli` — signal-cli daemon, registration, SPQR
- `wiki/decisions/` — architectural decisions that affect cluster operations
```

- [ ] **Step 2: Verify YAML/format**

Read the file back to confirm the section integrates cleanly with the existing structure.

- [ ] **Step 3: Commit**

```bash
cd ~/github_projects/zeroclaw
git add CLAUDE.md
git commit -m "docs: add knowledge routing section connecting zeroclaw to scrapyard wiki"
```

---

## Task 3: Migrate existing auto-memories to the wiki

**Files:**
- Read: `~/.claude/projects/-home-wsl2user-github-projects-zeroclaw/memory/*.md`
- Create/update: pages in `~/github_projects/scrapyard-wiki/wiki/`

Review each auto-memory and decide: wiki, auto-memory, or both.

| Memory | Current Location | Should Be | Action |
|--------|-----------------|-----------|--------|
| `feedback_shell_env_passthrough.md` | auto-memory | auto-memory | Keep (interaction preference) |
| `feedback_ambient_mesh_hbone.md` | auto-memory | wiki → `networking/ingress-and-mesh` | Migrate: add port 15008 note to existing networking page |
| `feedback_signal_cli_daemon_file_locks.md` | auto-memory | wiki → `services/signal-cli` (new) | Migrate: create signal-cli service page |
| `feedback_gemma4_tool_calling.md` | auto-memory | wiki → `services/zeroclaw` or `decisions/gemma4-migration` | Migrate: create decision page |
| `project_screenshot_error_postmortem.md` | auto-memory | wiki → `incidents/` | Migrate: create incident page |
| `project_walter_cron_loop_postmortem.md` | auto-memory | wiki → `incidents/` | Migrate: create incident page |
| `project_sam_signal_reregistration.md` | auto-memory | wiki → `services/signal-cli` | Migrate: merge into signal-cli service page |
| `project_gemma4_migration_complete.md` | auto-memory | wiki → `decisions/gemma4-migration` | Migrate: create decision page |
| `reference_agent_cli_tool_pattern.md` | auto-memory | wiki → `services/zeroclaw` | Migrate: add to zeroclaw service page |
| `reference_llama_swap_debugging.md` | auto-memory | wiki → `services/litellm` | Migrate: add to existing litellm page or create llama-swap page |
| `reference_gemma4_thinking_mode.md` | auto-memory | wiki → `decisions/gemma4-migration` | Migrate: merge into decision page |

- [ ] **Step 1: Create new wiki pages**

Use `/wiki-ingest` with "this session" mode, or create pages manually:

New pages needed:
- `wiki/services/signal-cli.md` — signal-cli daemon, registration, SPQR, Recreate strategy, file locks
- `wiki/services/zeroclaw.md` — ZeroClaw agent runtime, Sam deployment, Gemma 4 config, CLI tool pattern
- `wiki/decisions/gemma4-migration.md` — migration from Qwen 3.5 to Gemma 4, all constraints and config
- `wiki/incidents/2026-03-XX-screenshot-500-error.md` — screenshot postmortem
- `wiki/incidents/2026-03-XX-walter-cron-loop.md` — Walter cron loop postmortem

- [ ] **Step 2: Update existing wiki pages**

- `wiki/networking/ingress-and-mesh.md` — add HBONE port 15008 note
- `wiki/services/litellm.md` — add llama-swap debugging reference

- [ ] **Step 3: Update wiki index.md**

Add new pages to the appropriate sections.

- [ ] **Step 4: Slim down auto-memory**

After migration, replace migrated auto-memory files with one-line pointers:

```markdown
---
name: Signal CLI operations
description: Pointer to scrapyard wiki — see wiki/services/signal-cli
type: reference
---

Migrated to scrapyard wiki. See ~/github_projects/scrapyard-wiki/wiki/services/signal-cli.md
```

Or delete them entirely and remove from MEMORY.md index. Keep only:
- `feedback_shell_env_passthrough.md` — interaction preference, stays
- `feedback_gemma4_tool_calling.md` — rewrite as pointer to wiki decision page

- [ ] **Step 5: Update MEMORY.md index**

Reflect the slimmed-down state. The index becomes a thin routing layer:

```markdown
# Memory Index

## Feedback (interaction-specific, stays here)
- [shell_env_passthrough gotcha](feedback_shell_env_passthrough.md) — env vars need sandbox AND config.toml passthrough

## Wiki Pointers (knowledge lives in scrapyard wiki)
- Signal CLI operations → ~/github_projects/scrapyard-wiki/wiki/services/signal-cli.md
- ZeroClaw deployment → ~/github_projects/scrapyard-wiki/wiki/services/zeroclaw.md
- Gemma 4 migration → ~/github_projects/scrapyard-wiki/wiki/decisions/gemma4-migration.md
- Incidents → ~/github_projects/scrapyard-wiki/wiki/incidents/
```

- [ ] **Step 6: Commit wiki changes**

```bash
cd ~/github_projects/scrapyard-wiki
git add -A
git commit -m "wiki: ingest zeroclaw operational knowledge from auto-memory migration"
```

---

## Task 4: Add zeroclaw-specific pages to scrapyard wiki

**Files:**
- Create: `~/github_projects/scrapyard-wiki/wiki/services/zeroclaw.md`
- Create: `~/github_projects/scrapyard-wiki/wiki/services/signal-cli.md`
- Create: `~/github_projects/scrapyard-wiki/wiki/decisions/gemma4-migration.md`

These are the highest-value pages from today's session.

- [ ] **Step 1: Create wiki/services/zeroclaw.md**

Content should cover:
- What ZeroClaw is (agent runtime)
- Sam's deployment topology (pod with zeroclaw + serena + signal-api-adapter containers)
- ConfigMap architecture (identity, skills, main config)
- Gemma 4 presentation config (`flatten_json_responses`, `simplify_tool_schemas`, `show_reasoning`)
- Relationship to signal-cli daemon, LiteLLM, vLLM

- [ ] **Step 2: Create wiki/services/signal-cli.md**

Content should cover:
- signal-cli daemon deployment (v0.14.1, SPQR requirement)
- Recreate strategy (file lock contention)
- Registration process (must stop daemon first, CAPTCHA required)
- Account data on PVC, registration lock PIN
- Relationship to zeroclaw (signal-api-adapter bridge)

- [ ] **Step 3: Create wiki/decisions/gemma4-migration.md**

Content should cover:
- Context: Qwen 3.5 → Gemma 4 migration
- Key constraints (chevron sensitivity, tool response format, schema complexity, activation phrase)
- Architecture decisions (prompt guidance over hard constraints, JSON flattening, schema simplification)
- Config: `strip_prior_reasoning`, `flatten_json_responses`, `simplify_tool_schemas`, `show_reasoning`
- Thought stripping rules and function calling lifecycle

- [ ] **Step 4: Update wiki index.md**

Add the three new pages to the Services and Decisions sections.

- [ ] **Step 5: Commit**

```bash
cd ~/github_projects/scrapyard-wiki
git add -A
git commit -m "wiki: add zeroclaw, signal-cli, and gemma4-migration pages"
```

---

## Verification

After all tasks:

1. **Global CLAUDE.md** has clear routing rules (wiki vs auto-memory)
2. **Project CLAUDE.md** has a knowledge routing section pointing to the wiki
3. **Scrapyard wiki** has zeroclaw, signal-cli, and gemma4-migration pages
4. **Auto-memory** is slimmed to interaction-specific feedback + pointers to wiki
5. **No knowledge duplication** — each fact lives in exactly one place
6. **Future sessions** will naturally route knowledge to the right store because the CLAUDE.md instructions are clear

## Expected outcome

Next time I do cluster work in any project, I'll:
1. Check the wiki first (already instructed)
2. Save operational findings to the wiki (now clearly instructed)
3. Keep only interaction preferences in auto-memory (routing rule is explicit)
4. Run `/wiki-ingest` at session end for significant work (now part of the workflow)
