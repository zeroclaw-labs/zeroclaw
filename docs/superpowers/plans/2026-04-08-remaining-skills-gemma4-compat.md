# Remaining Skills — Gemma 4 Compatibility Rewrite

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite Sam's 3 remaining skill configmaps for Gemma 4 compatibility: K8s Delegation + Vikunja (shared configmap), Browser Navigation, and Science Curator.

**Architecture:** Each skill gets the same treatment: convert multi-paragraph prose to direct rule lists, remove example narratives, collapse markdown tables to inline text, eliminate conditional decision trees in favor of ordered preference lists. The K8s delegation skill is the largest (222 lines) and contains a work order template that must be preserved as a structural pattern. The Vikunja skill (68 lines) is already fairly compact. Browser Navigation (120 lines) has recovery patterns that compress well. Science Curator (67 lines) needs minimal changes.

**Tech Stack:** YAML (Kubernetes ConfigMap), Markdown

---

### Task 1: Rewrite K8s Delegation and Vikunja skills

**Files:**
- Modify: `k8s/sam/13_zeroclaw_skills_configmap.yaml`

This ConfigMap has two data keys: `k8s-delegation.md` (222 lines) and `vikunja-project-manager.md` (68 lines).

- [ ] **Step 1: Replace both skill contents**

Rewrite the entire file. The ConfigMap structure (apiVersion, kind, metadata with name `zeroclaw-skills` and namespace `ai-agents`) stays the same. Replace both data key values.

New `k8s-delegation.md` content:

```
---
name: k8s-delegation
version: 2.0.0
description: Delegate K8s tasks to Walter via ACP. Use for any cluster work.
always: false
---

# K8s Delegation to Walter

Walter is the K8s infrastructure agent. He has kubectl, the scrapyard-applications repo, and Serena memories. He has no access to your memory or conversation history.

## Core Rule

One work order = one deployable unit. Do not combine multiple steps.

## Work Order Format

Every acp-client send message must follow this structure:
TASK: one sentence describing what to do.
NAMESPACE: target namespace.
CONTEXT: what exists already, repo location, patterns to follow.
REQUIREMENTS: specific requirements as a list.
VERIFICATION: what to check when done (pod status, commit paths, service endpoints).
REPO PATH: path in scrapyard-applications repo.

## Deployment Sequence

Send each step as a separate work order. Verify before proceeding to the next.
1. Namespace with istio labels.
2. Secrets (VaultStaticSecret CRDs, if needed).
3. Storage (PVCs, if needed).
4. Database (CNPG PostgreSQL, if needed).
5. Application (Deployment + Service).
6. External access (Certificate + Gateway + VirtualService). Walter submits a PR for these. Wait for Dan to merge before testing.
7. Final verification.
Skip steps that do not apply.

## After Each Work Order

acp-client wait ID --timeout 600. Read response for concrete evidence (resource names, statuses, commits). If failed, send follow-up with error context. If successful, proceed. Clean up with acp-client delete ID.

## Monitoring

acp-client progress ID to check status. acp-client inject ID "message" to course-correct mid-task.

## Walter's PRs

When Walter submits a PR: note the PR number, tell Dan with the URL and what to do after merging. Do not send follow-up tasks that depend on PR'd resources until Dan confirms merge.

## PR Review Feedback

When Dan requests changes on Walter's PR: send Walter a task with the PR number, tell him to read comments with gitea-pr comments N, make changes, push, and reply.

## Do Not

Send multi-step plans as one message. Reference prior context Walter does not have. Skip verification between steps. Let Walter run 20+ minutes without checking progress.
```

New `vikunja-project-manager.md` content:

```
---
name: vikunja-project-manager
version: 2.0.0
description: Manage tasks via Vikunja CLI. Use for project status, task tracking, and TODO lists.
---

# Vikunja Project Manager

CLI tool on PATH, already authenticated. Run via shell (e.g., vikunja projects).

## Commands

vikunja projects — list all projects.
vikunja project create --title "..." — create project.
vikunja tasks PROJECT_ID — list tasks.
vikunja task create PROJECT_ID --title "..." [--description "..."] [--due "YYYY-MM-DD"] [--priority 1-5] [--assignee USERNAME] — create task.
vikunja task update TASK_ID [--done] [--title "..."] [--assignee USERNAME] — update task.
vikunja task assign TASK_ID --user USERNAME — assign task.
vikunja task comment TASK_ID --body "..." — add comment.

## Users

Known usernames: dan, sam, admin. Assign Dan to his action items, yourself to tasks you track.

## Workflow

New initiative: check if project exists first, create if needed, break into tasks with priorities and due dates.
Updating: vikunja tasks ID to see state, task update --done when complete, task comment for decisions or blockers.
Reporting: vikunja tasks ID, then summarize done/in-progress/blocked.

Priority 1 (lowest) through 5 (highest). Due dates: --due "YYYY-MM-DD". Task titles should be short and actionable.
```

- [ ] **Step 2: Validate YAML**

Run: `python3 -c "import yaml; data = yaml.safe_load(open('k8s/sam/13_zeroclaw_skills_configmap.yaml')); print('Keys:', sorted(data['data'].keys()))"`

Expected: `Keys: ['k8s-delegation.md', 'vikunja-project-manager.md']`

- [ ] **Step 3: Commit**

```bash
git add k8s/sam/13_zeroclaw_skills_configmap.yaml
git commit -m "refactor(k8s/sam): rewrite k8s-delegation and vikunja skills for Gemma 4"
```

---

### Task 2: Rewrite Browser Navigation skill

**Files:**
- Modify: `k8s/sam/23_browser_navigation_skill_configmap.yaml`

Current skill is 120 lines with decision frameworks, 3-tier element targeting, and 4 recovery scenarios.

- [ ] **Step 1: Replace the skill content**

Replace the entire `browser-navigation.md` data key value with:

```
---
name: browser-navigation
version: 2.0.0
description: Guides browser tool usage. Consult before calling the browser tool.
always: false
---

# Browser Navigation

The browser controls headless Chromium. Page state persists between calls.

## Browser vs Python

Use browser for: JavaScript-rendered pages, SPAs, clicking UIs, filling forms.
Use Python urllib via shell for: REST APIs, JSON endpoints, file downloads, predictable responses.
Python is faster and cheaper. Only use browser when rendering is required.

## Snapshot vs Screenshot

Snapshot: returns accessibility tree with ref identifiers for interaction. Primary tool for reading pages.
Screenshot: returns visual image. Use only for visual verification.
Always snapshot first on a new page. Use refs from snapshot to interact.

## Navigation Pattern

1. browser action=open url=TARGET
2. browser action=wait ms=2000
3. browser action=snapshot
If snapshot is empty, wait longer and retry. Check action=get_url to confirm location.

## Targeting Elements (order of preference)

1. Refs from snapshot: action=click selector="@e42". Most reliable.
2. Semantic find: action=find by=text value="Submit" find_action=click. Options for by: text, role, label, placeholder, testid.
3. CSS selectors: action=click selector="#submit-btn". Only when you know the exact selector.

## Recovery

Empty snapshot: wait longer, retry. Check get_url for redirects.
Element not found: scroll down (action=scroll direction=down pixels=500), snapshot again. Check for modal overlays.
Form submission fails: try action=press key=Enter instead of button click. Check for validation errors.
Login wall or CAPTCHA: report to Dan.

## Avoid

Screenshotting to read text (use snapshot or get_text). Guessing CSS selectors without a snapshot. Wrong ref format (use @e1 not ref=1). Rapid-fire actions without waits. Using browser for API calls. Letting snapshots accumulate (use action=close between sites).
```

- [ ] **Step 2: Validate YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('k8s/sam/23_browser_navigation_skill_configmap.yaml')); print('OK')"`

Expected: `OK`

- [ ] **Step 3: Commit**

```bash
git add k8s/sam/23_browser_navigation_skill_configmap.yaml
git commit -m "refactor(k8s/sam): rewrite browser navigation skill for Gemma 4 — 120 to 40 lines"
```

---

### Task 3: Rewrite Science Curator skill

**Files:**
- Modify: `k8s/sam/22_science_curator_skill_configmap.yaml`

Current skill is 67 lines. Already relatively compact — needs markdown table removed, prose tightened.

- [ ] **Step 1: Replace the skill content**

Replace the entire `science-curator.md` data key value with:

```
---
name: science-curator
version: 2.0.0
description: Curates science and space news. Use for science news requests or daily cron.
---

# Science News Curator

Curates science and space stories for Dan. Runs as daily cron or on-demand.

## Tool Routing

Use Python urllib via shell for all sources. Only use browser for JavaScript-rendered SPAs.

## Sources

Fetch recent stories (past week) from: NASA News (nasa.gov/news), ESA News (esa.int/News), Space.com, ScienceDaily (secondary, skip if others have enough). Use Python to fetch HTML and parse headlines.

## Process

1. Dedup: memory_recall key pattern science-news/ to check existing stories.
2. Fetch sources, identify 3-5 new stories not in memory.
3. Save each to memory with key science-news/YYYY-MM-DD/story-slug: headline, 2-3 sentence summary, why interesting, source URL.
4. Message Dan via Signal only if genuinely noteworthy (major discovery, mission milestone). Most days, save silently.

## Criteria

Prioritize: discoveries, mission milestones, technology breakthroughs.
Deprioritize: policy, budget, personnel, routine updates.

## Message Format (when messaging)

Keep scannable for mobile. Lead with date, then headline + one sentence per story. End with "Full details saved to memory."
```

- [ ] **Step 2: Validate YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('k8s/sam/22_science_curator_skill_configmap.yaml')); print('OK')"`

Expected: `OK`

- [ ] **Step 3: Commit**

```bash
git add k8s/sam/22_science_curator_skill_configmap.yaml
git commit -m "refactor(k8s/sam): rewrite science curator skill for Gemma 4 — 67 to 30 lines"
```

---

### Task 4: Deploy all three and validate

**Files:**
- Verify: `k8s/sam/13_zeroclaw_skills_configmap.yaml`
- Verify: `k8s/sam/22_science_curator_skill_configmap.yaml`
- Verify: `k8s/sam/23_browser_navigation_skill_configmap.yaml`

- [ ] **Step 1: Apply all to cluster**

```bash
kubectl apply -f k8s/sam/13_zeroclaw_skills_configmap.yaml
kubectl apply -f k8s/sam/22_science_curator_skill_configmap.yaml
kubectl apply -f k8s/sam/23_browser_navigation_skill_configmap.yaml
```

- [ ] **Step 2: Restart zeroclaw pod**

```bash
kubectl delete pod zeroclaw -n ai-agents
sleep 30
kubectl get pods -n ai-agents -l app=zeroclaw
```

Expected: Pod comes back 3/3 Running.

- [ ] **Step 3: Verify skills are mounted with new versions**

```bash
kubectl exec zeroclaw -n ai-agents -c zeroclaw -- head -3 /data/.zeroclaw/workspace/skills/k8s-delegation/SKILL.md
kubectl exec zeroclaw -n ai-agents -c zeroclaw -- head -3 /data/.zeroclaw/workspace/skills/browser-navigation/SKILL.md
kubectl exec zeroclaw -n ai-agents -c zeroclaw -- head -3 /data/.zeroclaw/workspace/skills/science-curator/SKILL.md
```

Expected: All show version 2.0.0.

---

## Reduction Summary

| Skill | Before (lines) | After (lines) | Reduction |
|-------|----------------|---------------|-----------|
| K8s Delegation | 222 | ~55 | 75% |
| Vikunja Project Manager | 68 | ~25 | 63% |
| Browser Navigation | 120 | ~40 | 67% |
| Science Curator | 67 | ~30 | 55% |
| **Total** | **477** | **~150** | **~69%** |

## Verification

- All YAML valid
- No chevrons in content
- Skills mounted on pod with version 2.0.0
- Pod restarts 3/3
