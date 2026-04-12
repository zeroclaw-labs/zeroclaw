# Agent Skill Organization Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reorganize Sam and Walter's task definitions into proper ZeroClaw SKILL.md files, referenced by identity files and cron jobs instead of inlined in ConfigMaps and cron prompts.

**Architecture:** ZeroClaw loads skills from `~/.zeroclaw/workspace/skills/<name>/SKILL.md` on startup. Each skill has YAML frontmatter (name, description, version, always) and markdown instructions. Skills are injected into the system prompt and can be referenced by cron prompts ("use the X skill") instead of duplicating instructions. The identity files (AGENTS.md, TOOLS.md) serve as the routing layer that tells the agent which skills exist and when to use them. This plan extracts inlined logic into standalone skills and thins the identity files into a skill directory.

**Tech Stack:** Kubernetes ConfigMaps, ZeroClaw SKILL.md format, Python (for cron seed scripts)

---

## Current State Analysis

### Sam (3 skills, 5 identity files, 4 cron jobs)

| Component | Location | Lines | Problem |
|-----------|----------|-------|---------|
| `daily-meeting-summary` skill | skills ConfigMap | ~240 | Well-structured but has a 7KB cron prompt inlined in the skill body that's duplicated into the cron DB |
| `k8s-delegation` skill | skills ConfigMap | ~350 | Good, references Walter via ACP |
| `vikunja-project-manager` skill | skills ConfigMap | ~55 | Good, lean |
| `TOOLS.md` identity | identity ConfigMap | 219 | Contains ACP usage docs, Serena reference, active projects — mixes tool reference with operational context |
| `AGENTS.md` identity | identity ConfigMap | 115 | Behavior guidelines + active project status |
| `speakr-daily-summary` cron | SQLite | 7KB prompt | Massive prompt that duplicates the Python script from the skill. If the skill is updated, the cron prompt is stale |
| `self-reflection-check` cron | SQLite | 208 chars | Simple, standalone — fine as-is |
| `vikunja-task-review` cron | SQLite | 496 chars | Recently rewritten, references vikunja CLI — fine as-is |
| `Daily Science & Space News Curator` cron | SQLite | 1.1KB | Self-contained, no matching skill — could be extracted |

### Walter (1 skill, 3 identity files, 1 cron job)

| Component | Location | Lines | Problem |
|-----------|----------|-------|---------|
| `k8s-manifest-builder` skill | skills ConfigMap | ~260 | Well-structured, recently refactored |
| `TOOLS.md` identity | identity ConfigMap | 127 | Contains manifest templates that belong in the skill |
| `IDENTITY.md` identity | identity ConfigMap | 54 | Clean |
| `AGENTS.md` identity | identity ConfigMap | 44 | Clean |
| `pr-review-monitor` cron | SQLite (seeded) | Short prompt | References skill by name — good pattern |

### Key Problems

1. **Cron prompts duplicate skill content.** The `speakr-daily-summary` cron has a 7KB prompt with the full Python script, but the same script is also in the skill. When we update the skill, the cron prompt goes stale.

2. **Identity files (TOOLS.md) contain skill-like content.** Sam's TOOLS.md has ACP usage docs, Serena memory reference, and project status. Walter's TOOLS.md has manifest templates. These should be in skills or reference files.

3. **No single source of truth for cron prompts.** Cron prompts live in SQLite (written once by seed scripts or Sam herself) and drift from the skill definitions over time.

4. **Skills ConfigMap is getting large.** Sam's is 642 lines across 3 skills. Adding more will hit readability limits.

---

## Target State

### Principles

1. **Skills are the single source of truth** for how to do a task. Cron prompts should say "use the X skill" not inline the full instructions.

2. **Identity files are routing tables**, not instruction manuals. TOOLS.md lists what tools exist and where to find docs. AGENTS.md defines behavior. Neither should contain procedural instructions.

3. **One ConfigMap per skill** for skills that change independently. This prevents a change to one skill from requiring a full ConfigMap redeploy.

4. **Cron seed scripts encode the "what" and "when"**, skills encode the "how".**

### File Map (Target)

```
k8s/sam/
  04_zeroclaw_sandbox.yaml          (modified — mount new skill ConfigMaps)
  05_zeroclaw_identity_configmap.yaml (modified — thin TOOLS.md and AGENTS.md)
  13_zeroclaw_skills_configmap.yaml  (modified — remove daily-meeting-summary, keep k8s-delegation)
  20_vikunja_tool_configmap.yaml     (unchanged)
  21_meeting_summary_skill_configmap.yaml  (new — extracted from skills ConfigMap)
  22_science_curator_skill_configmap.yaml  (new — extracted from cron prompt)

k8s/walter/
  02_identity_configmap.yaml  (modified — move templates from TOOLS.md to skill)
  03_sandbox.yaml             (modified — update cron seed prompt)
  04_skills_configmap.yaml    (modified — absorb template content from TOOLS.md)
```

---

## Chunk 1: Extract Sam's Meeting Summary Skill

### Task 1: Create standalone meeting summary skill ConfigMap

**Files:**
- Create: `k8s/sam/21_meeting_summary_skill_configmap.yaml`
- Modify: `k8s/sam/13_zeroclaw_skills_configmap.yaml` (remove `daily-meeting-summary.md` key)
- Modify: `k8s/sam/04_zeroclaw_sandbox.yaml` (add volume mount)

The `daily-meeting-summary` skill is the largest (240 lines) and changes most frequently. Moving it to its own ConfigMap means updates don't require redeploying the other skills.

- [ ] **Step 1: Extract the skill into its own ConfigMap**

Create `k8s/sam/21_meeting_summary_skill_configmap.yaml` with the full content of the `daily-meeting-summary.md` key from the skills ConfigMap. The ConfigMap key should be `daily-meeting-summary.md` and the structure should be:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: zeroclaw-skill-meeting-summary
  namespace: ai-agents
data:
  daily-meeting-summary.md: |
    ---
    name: daily-meeting-summary
    version: 2.3.0
    ...
    (full skill content)
```

- [ ] **Step 2: Remove the skill from the shared ConfigMap**

In `k8s/sam/13_zeroclaw_skills_configmap.yaml`, delete the entire `daily-meeting-summary.md: |` key and its content. This ConfigMap should retain only `k8s-delegation.md` and `vikunja-project-manager.md`.

- [ ] **Step 3: Add the new ConfigMap volume mount to the sandbox**

In `k8s/sam/04_zeroclaw_sandbox.yaml`, add to the init container's volume mounts:

```yaml
            - name: skill-meeting-summary
              mountPath: /etc/zeroclaw-template/skills-meeting-summary
              readOnly: true
```

And in the init container's shell script, add a copy block (after the existing skills loop):

```bash
# Sync standalone skill ConfigMaps.
for d in /etc/zeroclaw-template/skills-*/; do
  [ -d "$d" ] || continue
  for f in "$d"*.md; do
    [ -f "$f" ] || continue
    skill_name=$(basename "$f" .md)
    mkdir -p "/data/.zeroclaw/workspace/skills/$skill_name"
    cp "$f" "/data/.zeroclaw/workspace/skills/$skill_name/SKILL.md"
  done
done
```

And add to the `volumes` section:

```yaml
        - name: skill-meeting-summary
          configMap:
            name: zeroclaw-skill-meeting-summary
```

- [ ] **Step 4: Validate YAML**

```bash
python3 -c "import yaml; yaml.safe_load(open('k8s/sam/21_meeting_summary_skill_configmap.yaml')); print('OK')"
python3 -c "import yaml; yaml.safe_load(open('k8s/sam/13_zeroclaw_skills_configmap.yaml')); print('OK')"
python3 -c "import yaml; list(yaml.safe_load_all(open('k8s/sam/04_zeroclaw_sandbox.yaml'))); print('OK')"
```

- [ ] **Step 5: Apply and restart Sam**

```bash
kubectl apply -f k8s/sam/21_meeting_summary_skill_configmap.yaml
kubectl apply -f k8s/sam/13_zeroclaw_skills_configmap.yaml
kubectl apply -f k8s/sam/04_zeroclaw_sandbox.yaml
kubectl delete pod -n ai-agents zeroclaw
kubectl wait --for=condition=Ready pod/zeroclaw -n ai-agents --timeout=120s
```

- [ ] **Step 6: Verify skill is loaded**

```bash
kubectl logs -n ai-agents zeroclaw -c zeroclaw | grep "Skills:"
# Should show: daily-meeting-summary, k8s-delegation, vikunja-project-manager
```

- [ ] **Step 7: Commit**

```bash
git add k8s/sam/21_meeting_summary_skill_configmap.yaml k8s/sam/13_zeroclaw_skills_configmap.yaml k8s/sam/04_zeroclaw_sandbox.yaml
git commit -m "refactor(k8s/sam): extract meeting summary skill to standalone ConfigMap"
```

### Task 2: Make the cron prompt reference the skill instead of duplicating it

**Files:**
- Modify: Sam's cron DB (via kubectl exec)
- Modify: `k8s/sam/21_meeting_summary_skill_configmap.yaml` (update cron prompt section)

The current `speakr-daily-summary` cron has a 7KB prompt with the full Python script inlined. Instead, the cron prompt should be short and reference the skill. The Python script stays in the skill definition as the canonical source.

- [ ] **Step 1: Update the cron prompt in the DB**

```bash
kubectl exec -n ai-agents zeroclaw -c zeroclaw -- python3 -c "
import sqlite3
conn = sqlite3.connect('/data/.zeroclaw/workspace/cron/jobs.db')
prompt = '''You are running inside an isolated cron session.

Use the daily-meeting-summary skill to process today's meetings.
Follow Steps 1 through 3 from the Cron Job Prompt section of the skill.

Step 1: Run the Python aggregation script from the skill (copy it exactly).
Step 2: Read the aggregation file and write the executive summary.
Step 2.5: Create Vikunja tasks from the action items.
Step 3: Store the memory digest.

Do not call cron_run — you are already inside the cron job.'''
conn.execute('UPDATE cron_jobs SET prompt=? WHERE name=?', (prompt, 'speakr-daily-summary'))
conn.commit()
print('Updated speakr-daily-summary prompt')
conn.close()
"
```

- [ ] **Step 2: Verify the prompt was updated**

```bash
kubectl exec -n ai-agents zeroclaw -c zeroclaw -- python3 -c "
import sqlite3
conn = sqlite3.connect('/data/.zeroclaw/workspace/cron/jobs.db')
r = conn.execute('SELECT length(prompt) FROM cron_jobs WHERE name=\"speakr-daily-summary\"').fetchone()
print(f'Prompt length: {r[0]} chars (was 7022)')
conn.close()
"
```

Expected: ~350 chars (down from 7022).

- [ ] **Step 3: Commit**

```bash
git commit --allow-empty -m "docs(k8s/sam): update speakr cron to reference skill instead of inlining prompt

Note: cron prompt change was applied directly to Sam's DB via kubectl.
The skill ConfigMap is the canonical source of truth."
```

---

## Chunk 2: Extract Science Curator into a Skill

### Task 3: Create science curator skill

**Files:**
- Create: `k8s/sam/22_science_curator_skill_configmap.yaml`
- Modify: `k8s/sam/04_zeroclaw_sandbox.yaml` (add volume mount — handled by the glob pattern from Task 1 Step 3)

The `Daily Science & Space News Curator` cron has a 1.1KB prompt with no matching skill. This means:
- Sam can't be asked on-demand to "check science news" — there's no skill to guide her
- The cron prompt has no connection to the skill system

- [ ] **Step 1: Create the skill ConfigMap**

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: zeroclaw-skill-science-curator
  namespace: ai-agents
data:
  science-curator.md: |
    ---
    name: science-curator
    version: 1.0.0
    description: Curates science and space news from NASA, ESA, and other sources. Use when Dan asks about science news, space news, or interesting recent discoveries.
    ---

    # Science & Space News Curator

    You curate interesting science and space stories for Dan. This runs
    as a daily cron job but can also be triggered on-demand.

    ## Sources

    Check these sources for recent stories (past week):
    1. NASA News: https://www.nasa.gov/news
    2. ESA News: https://www.esa.int/News
    3. Space.com or ScienceDaily as secondary sources

    ## Process

    1. Visit each source and identify 3-5 particularly interesting stories
    2. For each story, save to memory with key `science-news/YYYY-MM-DD/story-slug`:
       - Headline and brief summary (2-3 sentences)
       - Why it's interesting
       - Source URL
    3. Send Dan a brief digest via Signal if anything is especially noteworthy

    ## Criteria for "interesting"

    Prioritize: discoveries, mission milestones, technology breakthroughs.
    Deprioritize: policy/budget news, personnel changes, routine updates.
```

- [ ] **Step 2: Add volume mount to sandbox**

In `k8s/sam/04_zeroclaw_sandbox.yaml`, add to volumes:

```yaml
        - name: skill-science-curator
          configMap:
            name: zeroclaw-skill-science-curator
```

And add the init container volume mount:

```yaml
            - name: skill-science-curator
              mountPath: /etc/zeroclaw-template/skills-science-curator
              readOnly: true
```

(The glob-based copy loop from Task 1 Step 3 will handle copying this to the skills directory.)

- [ ] **Step 3: Update the cron prompt to reference the skill**

```bash
kubectl exec -n ai-agents zeroclaw -c zeroclaw -- python3 -c "
import sqlite3
conn = sqlite3.connect('/data/.zeroclaw/workspace/cron/jobs.db')
prompt = 'Use the science-curator skill to find and save interesting science and space news stories from the past week.'
conn.execute('UPDATE cron_jobs SET prompt=? WHERE name=?', (prompt, 'Daily Science & Space News Curator'))
conn.commit()
print('Updated science curator cron prompt')
conn.close()
"
```

- [ ] **Step 4: Validate, apply, verify**

```bash
python3 -c "import yaml; yaml.safe_load(open('k8s/sam/22_science_curator_skill_configmap.yaml')); print('OK')"
kubectl apply -f k8s/sam/22_science_curator_skill_configmap.yaml
kubectl apply -f k8s/sam/04_zeroclaw_sandbox.yaml
kubectl delete pod -n ai-agents zeroclaw
kubectl wait --for=condition=Ready pod/zeroclaw -n ai-agents --timeout=120s
kubectl logs -n ai-agents zeroclaw -c zeroclaw | grep "Skills:"
# Should show: daily-meeting-summary, k8s-delegation, vikunja-project-manager, science-curator
```

- [ ] **Step 5: Commit**

```bash
git add k8s/sam/22_science_curator_skill_configmap.yaml k8s/sam/04_zeroclaw_sandbox.yaml
git commit -m "feat(k8s/sam): extract science curator into standalone skill"
```

---

## Chunk 3: Thin Sam's Identity Files

### Task 4: Refactor TOOLS.md to be a routing table

**Files:**
- Modify: `k8s/sam/05_zeroclaw_identity_configmap.yaml` (TOOLS.md key)

Sam's TOOLS.md is 219 lines and contains ACP usage docs, Serena memory reference, vision instructions, and active project status. Most of this belongs in skills or is duplicated by skill descriptions. TOOLS.md should be a concise reference that tells Sam what tools exist and points to skills for details.

- [ ] **Step 1: Rewrite TOOLS.md as a routing table**

Replace the TOOLS.md content in `k8s/sam/05_zeroclaw_identity_configmap.yaml` with a lean version (~60 lines):

```markdown
# TOOLS.md - Sam's Toolkit

## Native Tools
- `shell` — run commands, Python scripts, CLI tools (vikunja, acp-client)
- `file_read` / `file_write` — workspace filesystem
- `memory_store` / `memory_recall` — SQLite key-value memory
- `content_search` — full-text search across files
- `send_user_message` — send Signal messages to Dan
- `cron_list` / `cron_add` / `cron_remove` — manage scheduled jobs

## MCP Tools (Serena)
- Persistent cross-session memory: `write_memory`, `read_memory`, `list_memories`
- Code analysis: `find_symbol`, `get_symbols_overview`, `search_for_pattern`
- Use native memory for meetings/daily data. Use Serena memory for long-lived context.

## CLI Tools (on PATH)
- `vikunja` — project/task management (see vikunja-project-manager skill)
- `acp-client` — delegate K8s tasks to Walter (see k8s-delegation skill)

## Vision
You can see images. When Dan shares screenshots or photos, describe what you see.

## Output Discipline
- Keep shell output under 4KB. Use `head`, `tail`, or write to file for large output.
- Never dump raw JSON API responses into conversation — summarize.

## Key Principles
- Use the right tool for the job — check skills before improvising
- Ask Dan when unsure rather than guessing
- Keep conversations concise — Dan reads on mobile
```

- [ ] **Step 2: Trim AGENTS.md**

Remove any "active projects" or "current status" sections from AGENTS.md — these belong in Serena memory, not in the identity file. AGENTS.md should contain only behavior guidelines that don't change week to week.

- [ ] **Step 3: Validate and apply**

```bash
python3 -c "import yaml; yaml.safe_load(open('k8s/sam/05_zeroclaw_identity_configmap.yaml')); print('OK')"
kubectl apply -f k8s/sam/05_zeroclaw_identity_configmap.yaml
kubectl delete pod -n ai-agents zeroclaw
kubectl wait --for=condition=Ready pod/zeroclaw -n ai-agents --timeout=120s
```

- [ ] **Step 4: Commit**

```bash
git add k8s/sam/05_zeroclaw_identity_configmap.yaml
git commit -m "refactor(k8s/sam): thin TOOLS.md into routing table, remove project status from AGENTS.md"
```

---

## Chunk 4: Clean Up Walter's Identity

### Task 5: Move manifest templates from TOOLS.md to skill

**Files:**
- Modify: `k8s/walter/02_identity_configmap.yaml` (TOOLS.md key)
- Modify: `k8s/walter/04_skills_configmap.yaml` (absorb templates)

Walter's TOOLS.md has 127 lines including the manifest templates (Certificate, Gateway, VirtualService, VaultStaticSecret) that the skill references as "see TOOLS.md." Move the templates into the skill itself and make TOOLS.md a lean routing table.

- [ ] **Step 1: Add a "Deployment Patterns" reference section to the skill**

In `k8s/walter/04_skills_configmap.yaml`, add the manifest templates from TOOLS.md after the existing Step 3 (BUILD) section, under a `## Deployment Patterns` heading. This is what the skill already references ("Templates are in your TOOLS.md under Deployment Patterns").

- [ ] **Step 2: Rewrite Walter's TOOLS.md**

Replace the TOOLS.md content in `k8s/walter/02_identity_configmap.yaml` with a lean version:

```markdown
# TOOLS.md - Walter's Toolkit

## Native Tools
- `shell` — kubectl, git, gitea-pr CLI
- `file_read` / `file_write` — workspace filesystem

## MCP Tools (Serena)
- Persistent memory: `write_memory`, `read_memory`, `list_memories`

## CLI Tools (on PATH)
- `gitea-pr` — PR management on Gitea (see k8s-manifest-builder skill)
- `kubectl` — full cluster access in todolist namespace

## Key Principles
- Check existing state before creating resources
- One directory per app in the scrapyard repo
- Use VaultStaticSecret, never raw K8s Secrets
- See the k8s-manifest-builder skill for full workflow
```

- [ ] **Step 3: Update cron seed prompt to reference skill**

In `k8s/walter/08_cron_seed_configmap.yaml`, update the PROMPT to reference the skill:

```python
PROMPT = (
    "Run gitea-pr check-reviews to see if any of your open PRs have "
    "unaddressed review comments. If ACTION NEEDED is reported, use the "
    "k8s-manifest-builder skill's 'Addressing PR Review Comments' section "
    "(Phase READ through Phase REPORT) to address each PR's feedback. "
    "If all clear, no action needed."
)
```

- [ ] **Step 4: Validate, apply, restart Walter**

```bash
python3 -c "import yaml; yaml.safe_load(open('k8s/walter/02_identity_configmap.yaml')); print('OK')"
python3 -c "import yaml; yaml.safe_load(open('k8s/walter/04_skills_configmap.yaml')); print('OK')"
python3 -c "import yaml; yaml.safe_load(open('k8s/walter/08_cron_seed_configmap.yaml')); print('OK')"
kubectl apply -f k8s/walter/02_identity_configmap.yaml
kubectl apply -f k8s/walter/04_skills_configmap.yaml
kubectl apply -f k8s/walter/08_cron_seed_configmap.yaml
kubectl delete pod -n ai-agents zeroclaw-k8s-agent
kubectl wait --for=condition=Ready pod/zeroclaw-k8s-agent -n ai-agents --timeout=120s
```

- [ ] **Step 5: Re-seed cron (the seed script runs on startup, but verify)**

```bash
kubectl exec -n ai-agents zeroclaw-k8s-agent -c zeroclaw -- python3 -c "
import sqlite3
conn = sqlite3.connect('/data/.zeroclaw/workspace/cron/jobs.db')
r = conn.execute('SELECT prompt FROM cron_jobs WHERE name=\"pr-review-monitor\"').fetchone()
print(r[0][:100] if r else 'NOT FOUND')
conn.close()
"
```

- [ ] **Step 6: Commit**

```bash
git add k8s/walter/02_identity_configmap.yaml k8s/walter/04_skills_configmap.yaml k8s/walter/08_cron_seed_configmap.yaml
git commit -m "refactor(k8s/walter): move templates to skill, thin TOOLS.md, reference skill from cron"
```

- [ ] **Step 7: Push all changes**

```bash
git push
```

---

## Summary

| Task | What | Key outcome |
|------|------|-------------|
| 1 | Extract meeting summary skill | Standalone ConfigMap, independent deploys |
| 2 | Slim speakr cron prompt | 7KB → 350 chars, references skill |
| 3 | Extract science curator skill | New skill + slimmed cron prompt |
| 4 | Thin Sam's TOOLS.md | 219 → ~60 lines, routing table only |
| 5 | Clean Walter's identity | Templates in skill, TOOLS.md lean, cron references skill |

**Dependencies:** Task 1 must complete before Task 2 (cron needs skill to exist). Tasks 3-5 are independent of each other. Execute: 1 → 2, then 3/4/5 in parallel.

**Rollback:** Each task is a separate commit. Revert any independently.

**What this doesn't change:**
- The actual skill content (already well-written)
- The cron schedules
- The vikunja-project-manager skill (already standalone)
- Walter's k8s-manifest-builder skill content (already recently refactored)
