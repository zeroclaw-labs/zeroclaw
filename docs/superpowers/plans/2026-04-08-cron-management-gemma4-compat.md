# Cron Management Skill — Gemma 4 Compatibility Rewrite

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite Sam's cron management skill (`k8s/sam/24_cron_management_skill_configmap.yaml`) to be compact enough for Gemma 4 while preserving all operational constraints.

**Architecture:** The current skill is 312 lines with extensive prose explanations, infrastructure narratives, bad/good code examples, incident postmortems, and markdown tables. Gemma 4 needs direct rules, not explanatory context. The rewrite converts multi-paragraph guidance into categorized rule lists, removes narrative examples and incident stories, and collapses the parameters table into inline directives. The core rules are all preserved — just expressed more directly.

**Tech Stack:** YAML (Kubernetes ConfigMap), Markdown

---

### Task 1: Rewrite the cron-management.md skill

**Files:**
- Modify: `k8s/sam/24_cron_management_skill_configmap.yaml`

- [ ] **Step 1: Replace the cron-management.md content**

Replace the entire `cron-management.md` data key value with:

```yaml
  cron-management.md: |
    ---
    name: cron-management
    version: 2.0.0
    description: Guides creation, modification, and review of cron jobs. Consult before cron_add or cron_update.
    always: false
    ---

    # Cron Management

    Cron jobs persist in SQLite across restarts. A bad cron runs forever until removed.
    Consult this skill whenever you schedule, automate, or set up any recurring task.

    ## Before Creating a Cron

    1. Is this recurring or one-time? One-time tasks: just do it now, do not create a cron.
    2. Run cron_list. If you have 3+ agent crons, merge or replace before adding another.
    3. Default frequency: daily. More frequent than every 3 hours: ask Dan first.
    4. Define the exit condition: what does "nothing to do" look like? If unclear, rethink.
    5. Default delivery: none. Only use announce for output Dan needs every run. For conditional notifications, use send_user_message inside the prompt.

    ## Writing the Cron Prompt

    The prompt runs in an isolated session with no conversation history.
    It only sees: system prompt, MEMORY.md, and the cron prompt itself.

    Rules for every cron prompt:
    - Reference skills by name, not inline instructions. Skills update via ConfigMap; inlined logic fossilizes.
    - Reference entities by role, not name. Names change (Goose became Walter). Roles are stable.
    - Include an explicit exit condition: "If nothing to report: end immediately. Do not save memory, do not message Dan."
    - Use live tools (vikunja CLI, acp-client, shell), not just memory_recall. Memory is a cache that goes stale.
    - Constrain scope. Specify exactly what to check and what to do. Vague prompts cause runaway sessions.
    - Include "Do not call cron_run — you are already inside the cron job." in every agent prompt.

    ## cron_add Parameters

    name: descriptive and unique (speakr-daily-summary, vikunja-task-review).
    schedule: start conservative, daily over hourly. Timezone: America/Vancouver or America/Edmonton.
    job_type: agent for LLM reasoning, shell for deterministic scripts.
    session_target: almost always isolated. Use main only if conversation context is needed.
    prompt: follow the rules above. Read it back as if you have never seen it — does it make sense alone?

    ## Reviewing Existing Crons

    Check each job: still relevant? Prompt still accurate (names, endpoints, tools)? Frequency appropriate? Has exit condition? Uses live tools not just memory?
    Remove completed or abandoned crons. Downgrade overly frequent ones.

    ## Modifying a Cron

    Read the current prompt first via cron_list. Make targeted changes. Verify after updating.

    ## Shared Infrastructure

    You share a GPU backend (4 parallel slots) with other agents and interactive users.
    Each agent cron occupies a slot for 30-120 seconds. Multiple crons in the same window cause 429 rate limits and response delays.
    The scheduler applies 0-14 minute jitter automatically, but avoid scheduling many crons at the same hour.
    Sub-hourly agent crons almost never make sense. Use shell crons for fast checks instead.

    ## Frequency Guide

    Daily or twice-daily: summaries, task reviews, hygiene checks. Default choice.
    Every 3-6 hours: fast-moving state like PR review monitors.
    More frequent than 3 hours: ask Dan first.
    Sub-hourly: use a shell cron, not an agent cron.
```

Key changes:
- 312 lines → ~50 lines (84% reduction)
- Removed: 3 incident postmortems with narrative context (self-reflection, stale name, unbounded initiative)
- Removed: good/bad code examples with "Why it works" explanations
- Removed: markdown table for parameters (replaced with inline list)
- Removed: multi-paragraph infrastructure narrative (replaced with 4-line summary)
- Removed: "Writing the Cron Prompt" subsections (Reference skills, Reference entities, Include exit conditions, Include live verification, Constrain scope, Prevent infinite loops) — all merged into a single rule list
- Preserved: all 6 prompt rules, all 4 pre-creation checks, all 5 review checks, parameter guidance, frequency tiers, infrastructure awareness, slot budget concept, jitter note

- [ ] **Step 2: Verify YAML is valid**

Run: `python3 -c "import yaml; data = yaml.safe_load(open('k8s/sam/24_cron_management_skill_configmap.yaml')); print('Keys:', sorted(data['data'].keys())); print('Chars:', len(data['data']['cron-management.md']))"`

Expected: 1 key, ~2,000-2,500 chars (down from ~8,000+).

- [ ] **Step 3: Verify no chevrons in content**

Run: `grep -n '[<>]' k8s/sam/24_cron_management_skill_configmap.yaml | grep -v 'apiVersion\|kind:\|metadata:\|namespace:'`

Expected: No matches.

- [ ] **Step 4: Commit**

```bash
git add k8s/sam/24_cron_management_skill_configmap.yaml
git commit -m "refactor(k8s/sam): rewrite cron management skill for Gemma 4 — 312 to 50 lines"
```

---

### Task 2: Validate and deploy

**Files:**
- Verify: `k8s/sam/24_cron_management_skill_configmap.yaml`

- [ ] **Step 1: Apply to cluster**

```bash
kubectl apply -f k8s/sam/24_cron_management_skill_configmap.yaml
```

- [ ] **Step 2: Restart zeroclaw pod**

```bash
kubectl delete pod zeroclaw -n ai-agents
sleep 30
kubectl get pods -n ai-agents -l app=zeroclaw
```

Expected: Pod comes back 3/3 Running.

- [ ] **Step 3: Verify skill is mounted**

```bash
kubectl exec zeroclaw -n ai-agents -c zeroclaw -- cat /data/.zeroclaw/workspace/skills/cron-management/SKILL.md | head -5
```

Expected: Shows the new frontmatter with version 2.0.0.

- [ ] **Step 4: Commit (if fixes needed)**

```bash
git add k8s/sam/24_cron_management_skill_configmap.yaml
git commit -m "fix(k8s/sam): cron management deploy validation"
```

---

## Reduction Summary

| Component | Before | After | Reduction |
|-----------|--------|-------|-----------|
| Skill markdown | 312 lines | ~50 lines | 84% |
| Incident narratives | 3 stories (~30 lines) | 0 | 100% |
| Code examples | 6 blocks (~40 lines) | 0 | 100% |
| Parameter table | 12 lines | 5 inline lines | 58% |

## What was preserved (rule-level audit)

- Pre-creation checklist: all 4 questions (one-time vs recurring, cron count, frequency default, exit condition)
- Prompt rules: all 6 (reference skills not inline, reference roles not names, exit condition, live tools, constrain scope, no cron_run)
- Parameter guidance: all 6 parameters (name, schedule, job_type, session_target, prompt, delivery)
- Review checklist: all 5 checks (relevance, accuracy, frequency, exit condition, live tools)
- Infrastructure: slot count, saturation risk, jitter, frequency tiers
- Frequency guide: all 4 tiers (daily, 3-6h, ask Dan, sub-hourly=shell)

## Verification

- YAML valid
- No chevrons
- Skill mounted on pod
- Pod restarts 3/3
