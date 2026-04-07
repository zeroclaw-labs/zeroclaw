# ZeroClaw Docs Structure Map

This page defines the documentation structure across three axes:

1. Language
2. Part (category)
3. Function (document intent)

Last refreshed: **February 22, 2026**.

## 1) By Language

| Language | Entry point | Canonical tree | Notes |
|---|---|---|---|
| English | `docs/README.md` | `docs/` | All documentation is in English. |

## 2) By Part (Category)

These directories are the primary navigation modules by product area.

- `docs/getting-started/` for initial setup and first-run flows
- `docs/reference/` for command/config/provider/channel reference indexes
- `docs/operations/` for day-2 operations, deployment, and troubleshooting entry points
- `docs/security/` for security guidance and security-oriented navigation
- `docs/hardware/` for board/peripheral implementation and hardware workflows
- `docs/contributing/` for contribution and CI/review processes
- `docs/project/` for project snapshots, planning context, and status-oriented docs

## 3) By Function (Document Intent)

Use this grouping to decide where new docs belong.

### Runtime Contract (current behavior)

- `docs/commands-reference.md`
- `docs/providers-reference.md`
- `docs/channels-reference.md`
- `docs/config-reference.md`
- `docs/operations-runbook.md`
- `docs/troubleshooting.md`
- `docs/one-click-bootstrap.md`

### Setup / Integration Guides

- `docs/custom-providers.md`
- `docs/langgraph-integration.md`
- `docs/network-deployment.md`

### Policy / Process

- `docs/pr-workflow.md`
- `docs/reviewer-playbook.md`
- `docs/ci-map.md`
- `docs/actions-source-policy.md`

### Proposals / Roadmaps

- `docs/sandboxing.md`
- `docs/resource-limits.md`
- `docs/audit-logging.md`
- `docs/agnostic-security.md`
- `docs/frictionless-security.md`
- `docs/security-roadmap.md`

### Snapshots / Time-Bound Reports

- `docs/project-triage-snapshot-2026-02-18.md`

### Assets / Templates

- `docs/datasheets/`
- `docs/doc-template.md`

## Placement Rules (Quick)

- New runtime behavior docs must be linked from the appropriate category index and `docs/SUMMARY.md`.
