# Sam/Walter K8s Delegation Skills Architecture

## Overview
Two complementary skills enabling Sam to effectively delegate k8s tasks to Walter via ACP.

## Skills
1. **Sam: k8s-delegation** — Structures ACP work orders (TASK/NAMESPACE/CONTEXT/REQUIREMENTS/VERIFICATION). One message = one deployable unit. ConfigMap: `zeroclaw-skills` in `ai-agents` namespace.
2. **Walter: k8s-manifest-builder** — 7-step execution workflow (RECEIVE → CHECK → BUILD → APPLY → VERIFY → COMMIT → REPORT). ConfigMap: `zeroclaw-k8s-agent-skills` in `ai-agents` namespace.

## Deployment Pattern
- Skills are `.md` entries in ConfigMaps
- Mounted as volumes at `/etc/zeroclaw-template/skills/`
- Init container copies each `<name>.md` to `/data/.zeroclaw/workspace/skills/<name>/SKILL.md`
- Pod restart reloads skills

## Key Files (scrapyard-applications repo)
- Sam's skills ConfigMap: `04_scrapyard_test_projects/32_zeroclaw/13_zeroclaw_skills_configmap.yaml`
- Walter's skills ConfigMap: `04_scrapyard_test_projects/33_goose_todolist_sandbox/04_zeroclaw_k8s_agent/04_skills_configmap.yaml` (legacy path, dir predates Walter rename)
- Walter's sandbox (modified for skills sync): `04_scrapyard_test_projects/33_goose_todolist_sandbox/04_zeroclaw_k8s_agent/03_sandbox.yaml` (legacy path, dir predates Walter rename)

## Root Cause Addressed
Walter lost track because: (1) Sam's messages were vague/multi-step, (2) no structured execution workflow, (3) no verification loop. Created 2026-03-15.
