# Vikunja CLI Tool for Sam — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Sam (ZeroClaw agent) the ability to manage Vikunja projects and tasks via a CLI tool, enabling her to communicate project status through todolist.coffee-anon.com.

**Architecture:** A Python CLI script (`vikunja`) following the exact same pattern as Walter's `gitea-pr` tool — ConfigMap-mounted, uses `urllib` (no external deps), authenticates via env var from Vault. Network policy updated to allow Sam → Vikunja traffic. Skill added to Sam's skills ConfigMap to teach her when and how to use the tool.

**Tech Stack:** Python 3 (stdlib only), Kubernetes NetworkPolicy, VaultStaticSecret (VSO), ConfigMap

**Existing patterns to follow:**
- `k8s/walter/07_gitea_pr_configmap.yaml` — CLI tool in a ConfigMap
- `k8s/walter/03_sandbox.yaml` — init container copies tool to `/data/bin/`
- `k8s/sam/06_zeroclaw_networkpolicy.yaml` — egress rules
- `k8s/sam/02_zeroclaw_vso_vault.yaml` — Vault secret sync

---

## Task 1: Network Policy — Allow Sam → Vikunja

**Files:**
- Modify: `k8s/sam/06_zeroclaw_networkpolicy.yaml`

Sam's egress policy (`zeroclaw-allow-egress`) currently allows DNS, goose-subagent, goose-acp, and external HTTPS. Vikunja runs on port 3456 in the `todolist` namespace — Sam cannot reach it.

- [ ] **Step 1: Add Vikunja egress rule**

Add a new egress rule to `zeroclaw-allow-egress` in `k8s/sam/06_zeroclaw_networkpolicy.yaml`, between the goose-acp rule and the external HTTPS rule:

```yaml
    # Vikunja API (todolist project management)
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: todolist
          podSelector:
            matchLabels:
              app: vikunja
      ports:
        - protocol: TCP
          port: 3456
```

- [ ] **Step 2: Apply and verify**

```bash
kubectl apply -f k8s/sam/06_zeroclaw_networkpolicy.yaml
kubectl get networkpolicy zeroclaw-allow-egress -n ai-agents -o yaml | grep -A8 "3456"
```

Expected: The rule appears in the policy.

- [ ] **Step 3: Test connectivity from Sam's pod**

```bash
kubectl exec -n ai-agents zeroclaw -c zeroclaw -- python3 -c "
import urllib.request
req = urllib.request.Request('http://vikunja.todolist.svc.cluster.local:3456/api/v1/info')
resp = urllib.request.urlopen(req, timeout=5)
print(resp.read().decode()[:100])
"
```

Expected: JSON response with Vikunja version info.

- [ ] **Step 4: Commit**

```bash
git add k8s/sam/06_zeroclaw_networkpolicy.yaml
git commit -m "feat(k8s/sam): allow egress to vikunja on port 3456 in todolist namespace"
```

---

## Task 2: Vault Secret — Store Sam's Vikunja API Token

**Files:**
- Modify: Vault KV store (via CLI, not a manifest change)
- Modify: `k8s/sam/04_zeroclaw_sandbox.yaml` (add env var)

Sam needs her Vikunja credentials available as `VIKUNJA_API_TOKEN` and `VIKUNJA_BASE_URL` environment variables. The token is obtained by logging in to the Vikunja API with Sam's account and extracting the JWT.

The Vikunja API returns a JWT on login that serves as a long-lived API token. We'll generate one and store it in Vault at the existing `kvv2/zeroclaw/zeroclaw-config` path (same secret Sam already uses for `api_key` and `SPEAKR_API_TOKEN`).

- [ ] **Step 1: Generate a Vikunja API token for Sam**

From a pod that can reach Vikunja (e.g., a test pod in the todolist namespace), log in as Sam and extract the token:

```bash
kubectl run -n todolist token-gen --rm -i --restart=Never --image=curlimages/curl:8.5.0 -- \
  curl -s -X POST http://vikunja:3456/api/v1/login \
  -H "Content-Type: application/json" \
  -d '{"username": "sam", "password": "SamVikunjaAgent2026!"}'
```

Extract the `token` field from the JSON response.

- [ ] **Step 2: Store the token in Vault**

```bash
vault kv patch kvv2/zeroclaw/zeroclaw-config VIKUNJA_API_TOKEN="<token-from-step-1>"
```

Wait ~30s for VSO to sync, then verify:

```bash
kubectl get secret zeroclaw-config-secrets -n ai-agents -o jsonpath='{.data}' | python3 -c "
import sys, json
d = json.load(sys.stdin)
print('Has VIKUNJA_API_TOKEN:', 'VIKUNJA_API_TOKEN' in d)
"
```

- [ ] **Step 3: Add env vars to Sam's sandbox**

In `k8s/sam/04_zeroclaw_sandbox.yaml`, add to the zeroclaw container's `env` section (alongside existing `SPEAKR_API_TOKEN`):

```yaml
            - name: VIKUNJA_API_TOKEN
              valueFrom:
                secretKeyRef:
                  name: zeroclaw-config-secrets
                  key: VIKUNJA_API_TOKEN
                  optional: true
            - name: VIKUNJA_BASE_URL
              value: "http://vikunja.todolist.svc.cluster.local:3456"
```

- [ ] **Step 4: Apply and restart Sam**

```bash
kubectl apply -f k8s/sam/04_zeroclaw_sandbox.yaml
kubectl delete pod -n ai-agents zeroclaw
# Wait for pod to come back
kubectl wait --for=condition=Ready pod/zeroclaw -n ai-agents --timeout=120s
```

- [ ] **Step 5: Verify env var is available**

```bash
kubectl exec -n ai-agents zeroclaw -c zeroclaw -- python3 -c "
import os
print('VIKUNJA_API_TOKEN set:', bool(os.environ.get('VIKUNJA_API_TOKEN')))
print('VIKUNJA_BASE_URL:', os.environ.get('VIKUNJA_BASE_URL', 'NOT SET'))
"
```

- [ ] **Step 6: Commit**

```bash
git add k8s/sam/04_zeroclaw_sandbox.yaml
git commit -m "feat(k8s/sam): add VIKUNJA_API_TOKEN and VIKUNJA_BASE_URL env vars"
```

---

## Task 3: CLI Tool — `vikunja` Python Script

**Files:**
- Create: `k8s/sam/20_vikunja_tool_configmap.yaml`
- Modify: `k8s/sam/04_zeroclaw_sandbox.yaml` (add volume mount + init container copy)

This follows the exact pattern of Walter's `gitea-pr` tool:
1. Python script in a ConfigMap
2. Mounted into an init container volume
3. Copied to `/data/bin/` (on PATH) during init
4. Called as `vikunja <command>` from the shell tool

### Step 3a: Write the CLI script

- [ ] **Step 1: Create the ConfigMap**

Create `k8s/sam/20_vikunja_tool_configmap.yaml`:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: zeroclaw-vikunja-tool
  namespace: ai-agents
data:
  vikunja.py: |
    #!/usr/bin/env python3
    """Vikunja CLI for Sam (ZeroClaw Agent).

    Manages projects and tasks on the Vikunja instance at todolist.coffee-anon.com
    for project status tracking and coordination.

    Commands:
      vikunja projects                        List all projects
      vikunja project create --title "..."    Create a project
      vikunja tasks <project-id>              List tasks in a project
      vikunja task create <project-id> --title "..." [--description "..."] [--due "YYYY-MM-DD"]
      vikunja task update <task-id> [--done] [--title "..."] [--description "..."]
      vikunja task comment <task-id> --body "..."
      vikunja help

    Environment:
      VIKUNJA_API_TOKEN  — JWT token for the sam user (required)
      VIKUNJA_BASE_URL   — Vikunja URL (default: http://vikunja.todolist.svc.cluster.local:3456)
    """
    import json
    import os
    import sys
    import urllib.request
    import urllib.error

    BASE_URL = os.environ.get("VIKUNJA_BASE_URL", "http://vikunja.todolist.svc.cluster.local:3456")
    API_URL = f"{BASE_URL}/api/v1"
    TOKEN = os.environ.get("VIKUNJA_API_TOKEN", "")


    def _api(method, path, data=None, timeout=15):
        """Make an authenticated Vikunja API request."""
        if not TOKEN:
            print("ERROR: VIKUNJA_API_TOKEN not set", file=sys.stderr)
            sys.exit(1)
        url = f"{API_URL}{path}"
        headers = {
            "Authorization": f"Bearer {TOKEN}",
            "Content-Type": "application/json",
        }
        body = json.dumps(data).encode() if data else None
        req = urllib.request.Request(url, data=body, headers=headers, method=method)
        try:
            resp = urllib.request.urlopen(req, timeout=timeout)
            raw = resp.read().decode()
            return json.loads(raw) if raw else {}
        except urllib.error.HTTPError as e:
            error_body = e.read().decode() if e.fp else ""
            print(f"ERROR: HTTP {e.code} — {e.reason}", file=sys.stderr)
            if error_body:
                try:
                    detail = json.loads(error_body)
                    print(f"Detail: {detail.get('message', error_body)}", file=sys.stderr)
                except json.JSONDecodeError:
                    print(f"Detail: {error_body[:500]}", file=sys.stderr)
            sys.exit(1)
        except urllib.error.URLError as e:
            print(f"ERROR: {e.reason}", file=sys.stderr)
            sys.exit(1)


    def _parse_args(args, flags):
        """Parse --flag value pairs from args list. Returns (parsed_dict, positional_list)."""
        parsed = {}
        positional = []
        i = 0
        while i < len(args):
            if args[i].startswith("--"):
                key = args[i][2:]
                if key in flags and flags[key] == "bool":
                    parsed[key] = True
                    i += 1
                elif key in flags and i + 1 < len(args):
                    parsed[key] = args[i + 1]
                    i += 2
                else:
                    print(f"Unknown or incomplete flag: {args[i]}", file=sys.stderr)
                    sys.exit(1)
            else:
                positional.append(args[i])
                i += 1
        return parsed, positional


    def cmd_projects(args):
        """List all projects."""
        projects = _api("GET", "/projects")
        if not projects:
            print("No projects found.")
            return
        for p in projects:
            pid = p.get("id", "?")
            title = p.get("title", "Untitled")
            desc = p.get("description", "")
            desc_preview = f" — {desc[:60]}" if desc else ""
            print(f"  #{pid}: {title}{desc_preview}")


    def cmd_project_create(args):
        """Create a new project."""
        flags, _ = _parse_args(args, {"title": "str", "description": "str"})
        title = flags.get("title")
        if not title:
            print('Usage: vikunja project create --title "..." [--description "..."]', file=sys.stderr)
            sys.exit(1)
        data = {"title": title}
        if "description" in flags:
            data["description"] = flags["description"]
        result = _api("PUT", "/projects", data)
        print(f"Project #{result.get('id', '?')} created: {result.get('title', title)}")


    def cmd_tasks(args):
        """List tasks in a project."""
        if not args:
            print("Usage: vikunja tasks <project-id>", file=sys.stderr)
            sys.exit(1)
        project_id = args[0]
        # Use filter endpoint for project tasks
        tasks = _api("GET", f"/projects/{project_id}/views")
        # Get the list view ID (first view)
        if not tasks:
            print("No views found for this project.")
            return
        view_id = tasks[0].get("id")
        task_list = _api("GET", f"/projects/{project_id}/views/{view_id}/tasks")
        if not task_list:
            print(f"No tasks in project #{project_id}.")
            return
        for t in task_list:
            tid = t.get("id", "?")
            title = t.get("title", "Untitled")
            done = "DONE" if t.get("done") else "TODO"
            due = t.get("due_date", "")
            due_str = f" (due: {due[:10]})" if due and due != "0001-01-01T00:00:00Z" else ""
            priority = t.get("priority", 0)
            pri_str = f" [P{priority}]" if priority else ""
            print(f"  #{tid} [{done}]{pri_str} {title}{due_str}")


    def cmd_task_create(args):
        """Create a task in a project."""
        flags, positional = _parse_args(args, {
            "title": "str", "description": "str", "due": "str",
            "priority": "str",
        })
        if not positional:
            print('Usage: vikunja task create <project-id> --title "..." [--description "..."] [--due "YYYY-MM-DD"] [--priority 1-5]',
                  file=sys.stderr)
            sys.exit(1)
        project_id = positional[0]
        title = flags.get("title")
        if not title:
            print("ERROR: --title is required", file=sys.stderr)
            sys.exit(1)
        data = {"title": title, "project_id": int(project_id)}
        if "description" in flags:
            data["description"] = flags["description"]
        if "due" in flags:
            data["due_date"] = flags["due"] + "T00:00:00Z"
        if "priority" in flags:
            data["priority"] = int(flags["priority"])
        result = _api("PUT", f"/projects/{project_id}/tasks", data)
        print(f"Task #{result.get('id', '?')} created: {result.get('title', title)}")


    def cmd_task_update(args):
        """Update a task."""
        flags, positional = _parse_args(args, {
            "done": "bool", "title": "str", "description": "str",
            "due": "str", "priority": "str",
        })
        if not positional:
            print('Usage: vikunja task update <task-id> [--done] [--title "..."] [--description "..."]',
                  file=sys.stderr)
            sys.exit(1)
        task_id = positional[0]
        # Fetch current task state first
        current = _api("GET", f"/tasks/{task_id}")
        data = {}
        if flags.get("done"):
            data["done"] = True
        if "title" in flags:
            data["title"] = flags["title"]
        if "description" in flags:
            data["description"] = flags["description"]
        if "due" in flags:
            data["due_date"] = flags["due"] + "T00:00:00Z"
        if "priority" in flags:
            data["priority"] = int(flags["priority"])
        if not data:
            print("Nothing to update. Use --done, --title, --description, --due, or --priority.",
                  file=sys.stderr)
            sys.exit(1)
        result = _api("POST", f"/tasks/{task_id}", data)
        status = "DONE" if result.get("done") else "TODO"
        print(f"Task #{task_id} updated [{status}]: {result.get('title', '?')}")


    def cmd_task_comment(args):
        """Add a comment to a task."""
        flags, positional = _parse_args(args, {"body": "str"})
        if not positional or "body" not in flags:
            print('Usage: vikunja task comment <task-id> --body "..."', file=sys.stderr)
            sys.exit(1)
        task_id = positional[0]
        result = _api("PUT", f"/tasks/{task_id}/comments", {"comment": flags["body"]})
        print(f"Comment added to task #{task_id} (comment id: {result.get('id', '?')})")


    def main():
        if len(sys.argv) < 2 or sys.argv[1] in ("help", "--help", "-h"):
            print(__doc__)
            sys.exit(0)

        cmd = sys.argv[1]
        rest = sys.argv[2:]

        if cmd == "projects":
            cmd_projects(rest)
        elif cmd == "project" and rest and rest[0] == "create":
            cmd_project_create(rest[1:])
        elif cmd == "tasks":
            cmd_tasks(rest)
        elif cmd == "task" and rest:
            subcmd = rest[0]
            if subcmd == "create":
                cmd_task_create(rest[1:])
            elif subcmd == "update":
                cmd_task_update(rest[1:])
            elif subcmd == "comment":
                cmd_task_comment(rest[1:])
            else:
                print(f"Unknown task subcommand: {subcmd}", file=sys.stderr)
                sys.exit(1)
        else:
            print(f"Unknown command: {cmd}", file=sys.stderr)
            print(__doc__)
            sys.exit(1)


    if __name__ == "__main__":
        main()
```

- [ ] **Step 2: Validate YAML**

```bash
python3 -c "import yaml; yaml.safe_load(open('k8s/sam/20_vikunja_tool_configmap.yaml')); print('OK')"
```

### Step 3b: Mount and install the tool

- [ ] **Step 3: Add volume and volume mount to Sam's sandbox**

In `k8s/sam/04_zeroclaw_sandbox.yaml`:

Add to the `volumes` section:

```yaml
        - name: vikunja-tool
          configMap:
            name: zeroclaw-vikunja-tool
```

Add to the init container's `volumeMounts`:

```yaml
            - name: vikunja-tool
              mountPath: /etc/zeroclaw-template/vikunja
              readOnly: true
```

Add to the init container's shell script (after the existing tool copy commands):

```bash
# Copy vikunja tool to /data/bin (on PATH).
if [ -f /etc/zeroclaw-template/vikunja/vikunja.py ]; then
  echo "Installing vikunja tool"
  cp /etc/zeroclaw-template/vikunja/vikunja.py /data/bin/vikunja
  chmod +x /data/bin/vikunja
fi
```

- [ ] **Step 4: Apply both ConfigMaps and restart**

```bash
kubectl apply -f k8s/sam/20_vikunja_tool_configmap.yaml
kubectl apply -f k8s/sam/04_zeroclaw_sandbox.yaml
kubectl delete pod -n ai-agents zeroclaw
kubectl wait --for=condition=Ready pod/zeroclaw -n ai-agents --timeout=120s
```

- [ ] **Step 5: Verify tool is installed and works**

```bash
kubectl exec -n ai-agents zeroclaw -c zeroclaw -- vikunja help
kubectl exec -n ai-agents zeroclaw -c zeroclaw -- vikunja projects
```

Expected: Help text prints, projects list shows the "ZeroClaw Status" project.

- [ ] **Step 6: End-to-end test — create a task**

```bash
kubectl exec -n ai-agents zeroclaw -c zeroclaw -- vikunja task create 4 --title "Test task from Sam" --description "Verifying vikunja CLI integration"
kubectl exec -n ai-agents zeroclaw -c zeroclaw -- vikunja tasks 4
```

Expected: Task is created and appears in the list.

- [ ] **Step 7: Clean up test data and commit**

```bash
# Note the task ID from the previous step, then:
kubectl exec -n ai-agents zeroclaw -c zeroclaw -- vikunja task update <task-id> --done

git add k8s/sam/20_vikunja_tool_configmap.yaml k8s/sam/04_zeroclaw_sandbox.yaml
git commit -m "feat(k8s/sam): add vikunja CLI tool for project status management"
```

---

## Task 4: Sam Skill — Teach Sam to Use the Tool

**Files:**
- Modify: `k8s/sam/13_zeroclaw_skills_configmap.yaml`

Sam needs a skill that tells her when and how to use the `vikunja` CLI. This follows the skill-creator best practices: explain the why, keep it lean, use imperative form.

- [ ] **Step 1: Add vikunja-project-manager skill**

Add a new skill entry to `k8s/sam/13_zeroclaw_skills_configmap.yaml`. The skill should cover:

- When to use: Dan asks about project status, task tracking, creating/updating tasks, or communicating progress
- How to authenticate: `VIKUNJA_API_TOKEN` is already in the environment
- Command reference: the full `vikunja help` output
- Workflow: how to structure project updates (create project → create tasks → update as work completes → comment with context)

```yaml
  vikunja-project-manager.md: |
    ---
    name: vikunja-project-manager
    version: 1.0.0
    description: Manage project status and tasks via the Vikunja CLI. Use whenever Dan asks about project status, task tracking, TODO lists, or wants to communicate progress on work items. Also use when you complete work and want to update project status proactively.
    ---

    # Vikunja Project Manager

    You have access to a `vikunja` CLI tool that manages projects and tasks
    on the team's Vikunja instance (todolist.coffee-anon.com). Use it to
    track work status, create actionable task lists, and report progress.

    ## When to use this

    - Dan asks about project status or what's being worked on
    - You complete a piece of work and want to record it
    - Dan asks you to create a task list or project plan
    - You need to check what's outstanding before starting work

    ## Commands

    ```
    vikunja projects                        # List all projects
    vikunja project create --title "..."    # Create a new project
    vikunja tasks <project-id>              # List tasks in a project
    vikunja task create <project-id> --title "..." [--description "..."] [--due "YYYY-MM-DD"] [--priority 1-5]
    vikunja task update <task-id> [--done] [--title "..."] [--description "..."]
    vikunja task comment <task-id> --body "..."
    ```

    ## Workflow

    **Starting a new initiative:**
    1. `vikunja project create --title "Initiative Name"`
    2. Break work into tasks: `vikunja task create <id> --title "..." --priority 3`
    3. Report the project ID and task list to Dan

    **Updating progress:**
    1. `vikunja tasks <project-id>` to see current state
    2. `vikunja task update <id> --done` when work is complete
    3. `vikunja task comment <id> --body "context about what was done"`

    **Reporting status:**
    1. `vikunja tasks <project-id>` to get the full list
    2. Summarize: what's done, what's in progress, what's blocked

    ## Tips

    - Use priority 1 (lowest) through 5 (highest) to flag urgency
    - Add due dates for time-sensitive work: `--due "2026-03-20"`
    - Comments on tasks are useful for recording decisions or blockers
    - Keep task titles short and actionable ("Deploy vikunja postgres", not "We need to set up the database")
```

- [ ] **Step 2: Validate YAML**

```bash
python3 -c "import yaml; yaml.safe_load(open('k8s/sam/13_zeroclaw_skills_configmap.yaml')); print('OK')"
```

- [ ] **Step 3: Apply and restart**

```bash
kubectl apply -f k8s/sam/13_zeroclaw_skills_configmap.yaml
kubectl delete pod -n ai-agents zeroclaw
kubectl wait --for=condition=Ready pod/zeroclaw -n ai-agents --timeout=120s
```

- [ ] **Step 4: Commit**

```bash
git add k8s/sam/13_zeroclaw_skills_configmap.yaml
git commit -m "feat(k8s/sam): add vikunja-project-manager skill"
```

---

## Summary

| Task | What | Files | Risk |
|------|------|-------|------|
| 1 | Network policy: Sam → Vikunja | `k8s/sam/06_zeroclaw_networkpolicy.yaml` | Low |
| 2 | Vault secret + env var | Vault CLI + `k8s/sam/04_zeroclaw_sandbox.yaml` | Low |
| 3 | CLI tool script + mount | `k8s/sam/20_vikunja_tool_configmap.yaml` + `k8s/sam/04_zeroclaw_sandbox.yaml` | Medium |
| 4 | Skill for Sam | `k8s/sam/13_zeroclaw_skills_configmap.yaml` | Low |

**Dependencies:** Task 1 must complete before Task 3 can be tested. Task 2 must complete before Task 3 can authenticate. Task 3 must complete before Task 4 is useful. Execute sequentially: 1 → 2 → 3 → 4.

**Rollback:** Each task is a separate commit. Revert any commit independently. Network policy and env var changes take effect on next pod restart. ConfigMap changes require pod restart.
