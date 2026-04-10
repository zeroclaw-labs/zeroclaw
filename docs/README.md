# ZeroClaw Documentation Hub

This page is the primary entry point for the documentation system.

Last refreshed: **February 21, 2026**.

## Start Here

| I want to… | Read this |
|---|---|
| Install and run ZeroClaw quickly | [README.md (Quick Start)](../README.md#quick-start) |
| Bootstrap in one command | [one-click-bootstrap.md](setup-guides/one-click-bootstrap.md) |
| Update or uninstall on macOS | [macos-update-uninstall.md](setup-guides/macos-update-uninstall.md) |
| Find commands by task | [commands-reference.md](reference/cli/commands-reference.md) |
| Check config defaults and keys quickly | [config-reference.md](reference/api/config-reference.md) |
| Configure custom providers/endpoints | [custom-providers.md](contributing/custom-providers.md) |
| Use LangGraph integration patterns | [langgraph-integration.md](contributing/langgraph-integration.md) |
| Operate runtime (day-2 runbook) | [operations-runbook.md](ops/operations-runbook.md) |
| Troubleshoot install/runtime/channel issues | [troubleshooting.md](ops/troubleshooting.md) |
| Browse docs by category | [SUMMARY.md](SUMMARY.md) |
| See project PR/issue docs snapshot | [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md) |

## Quick Decision Tree (10 seconds)

- Need first-time setup or install? → [setup-guides/README.md](setup-guides/README.md)
- Need exact CLI/config keys? → [reference/README.md](reference/README.md)
- Need production/service operations? → [ops/README.md](ops/README.md)
- Seeing failures or regressions? → [troubleshooting.md](ops/troubleshooting.md)
- Working on security hardening or roadmap? → [security/README.md](security/README.md)
- Contributing/reviewing/CI workflow? → [contributing/README.md](contributing/README.md)
- Want the full map? → [SUMMARY.md](SUMMARY.md)

## Collections (Recommended)

- Getting started: [setup-guides/README.md](setup-guides/README.md)
- Reference catalogs: [reference/README.md](reference/README.md)
- Operations & deployment: [ops/README.md](ops/README.md)
- Security docs: [security/README.md](security/README.md)
- Contributing/CI: [contributing/README.md](contributing/README.md)
- Project snapshots: [maintainers/README.md](maintainers/README.md)

## By Audience

### Users / Operators

- [commands-reference.md](reference/cli/commands-reference.md) — command lookup by workflow
- [providers-reference.md](reference/api/providers-reference.md) — provider IDs, aliases, credential env vars
- [channels-reference.md](reference/api/channels-reference.md) — channel capabilities and setup paths
- [config-reference.md](reference/api/config-reference.md) — high-signal config keys and secure defaults
- [custom-providers.md](contributing/custom-providers.md) — custom provider/base URL integration templates
- [langgraph-integration.md](contributing/langgraph-integration.md) — fallback integration for model/tool-calling edge cases
- [operations-runbook.md](ops/operations-runbook.md) — day-2 runtime operations and rollback flow
- [troubleshooting.md](ops/troubleshooting.md) — common failure signatures and recovery steps

### Contributors / Maintainers

- [../CONTRIBUTING.md](../CONTRIBUTING.md)
- [pr-workflow.md](contributing/pr-workflow.md)
- [reviewer-playbook.md](contributing/reviewer-playbook.md)
- [ci-map.md](contributing/ci-map.md)
- [actions-source-policy.md](contributing/actions-source-policy.md)

### Security / Reliability

> Note: this area includes proposal/roadmap docs. For current behavior, start with [config-reference.md](reference/api/config-reference.md), [operations-runbook.md](ops/operations-runbook.md), and [troubleshooting.md](ops/troubleshooting.md).

- [security/README.md](security/README.md)
- [agnostic-security.md](security/agnostic-security.md)
- [frictionless-security.md](security/frictionless-security.md)
- [sandboxing.md](security/sandboxing.md)
- [audit-logging.md](security/audit-logging.md)
- [resource-limits.md](ops/resource-limits.md)
- [security-roadmap.md](security/security-roadmap.md)

## System Navigation & Governance

- Unified TOC: [SUMMARY.md](SUMMARY.md)
- Docs structure map (language/part/function): [structure/README.md](maintainers/structure-README.md)
- Documentation inventory/classification: [docs-inventory.md](maintainers/docs-inventory.md)
- Project triage snapshot: [project-triage-snapshot-2026-02-18.md](maintainers/project-triage-snapshot-2026-02-18.md)
