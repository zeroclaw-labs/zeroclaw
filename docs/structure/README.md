# ZeroClaw Docs Structure Map

This page defines the documentation structure across three axes:

1. Language
2. Part (category)
3. Function (document intent)

Last refreshed: **February 22, 2026**.

## 1) By Language

| Language | Entry point | Canonical tree | Notes |
|---|---|---|---|
| English | `docs/README.md` | `docs/` | Source-of-truth runtime behavior docs are authored in English first. |
| Chinese (`zh-CN`) | `docs/README.zh-CN.md` | `docs/` localized hub + selected localized docs | Uses localized hub and shared category structure. |
| Japanese (`ja`) | `docs/README.ja.md` | `docs/` localized hub + selected localized docs | Uses localized hub and shared category structure. |
| Russian (`ru`) | `docs/README.ru.md` | `docs/` localized hub + selected localized docs | Uses localized hub and shared category structure. |
| French (`fr`) | `docs/README.fr.md` | `docs/` localized hub + selected localized docs | Uses localized hub and shared category structure. |
| Vietnamese (`vi`) | `docs/i18n/vi/README.md` | `docs/i18n/vi/` | Full Vietnamese tree is canonical under `docs/i18n/vi/`; `docs/vi/` and `docs/*.vi.md` are compatibility paths. |

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
- `docs/zai-glm-setup.md`
- `docs/langgraph-integration.md`
- `docs/network-deployment.md`
- `docs/matrix-e2ee-guide.md`
- `docs/mattermost-setup.md`
- `docs/nextcloud-talk-setup.md`

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
- Navigation changes must preserve locale parity across `docs/README*.md` and `docs/SUMMARY*.md`.
- Vietnamese full localization lives in `docs/i18n/vi/`; compatibility files should point to canonical paths.
