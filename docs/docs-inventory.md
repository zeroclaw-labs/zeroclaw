# ZeroClaw Documentation Inventory

This inventory classifies docs by intent so readers can quickly distinguish runtime-contract guides from design proposals.

Last reviewed: **February 18, 2026**.

## Classification Legend

- **Current Guide/Reference**: intended to match current runtime behavior
- **Policy/Process**: collaboration or governance rules
- **Proposal/Roadmap**: design exploration; may include hypothetical commands
- **Snapshot**: time-bound operational report

## Documentation Entry Points

| Doc | Type | Audience |
|---|---|---|
| `README.md` | Current Guide | all readers |
| `README.zh-CN.md` | Current Guide (localized) | Chinese readers |
| `README.ja.md` | Current Guide (localized) | Japanese readers |
| `README.ru.md` | Current Guide (localized) | Russian readers |
| `README.vi.md` | Current Guide (localized) | Vietnamese readers |
| `docs/README.md` | Current Guide (hub) | all readers |
| `docs/README.zh-CN.md` | Current Guide (localized hub) | Chinese readers |
| `docs/README.ja.md` | Current Guide (localized hub) | Japanese readers |
| `docs/README.ru.md` | Current Guide (localized hub) | Russian readers |
| `docs/README.vi.md` | Current Guide (localized hub) | Vietnamese readers |
| `docs/SUMMARY.md` | Current Guide (unified TOC) | all readers |
| `docs/structure/README.md` | Current Guide (structure map) | all readers |

## Collection Index Docs

| Doc | Type | Audience |
|---|---|---|
| `docs/getting-started/README.md` | Current Guide | new users |
| `docs/reference/README.md` | Current Guide | users/operators |
| `docs/operations/README.md` | Current Guide | operators |
| `docs/security/README.md` | Current Guide | operators/contributors |
| `docs/hardware/README.md` | Current Guide | hardware builders |
| `docs/contributing/README.md` | Current Guide | contributors/reviewers |
| `docs/project/README.md` | Current Guide | maintainers |

## Current Guides & References

| Doc | Type | Audience |
|---|---|---|
| `docs/one-click-bootstrap.md` | Current Guide | users/operators |
| `docs/commands-reference.md` | Current Reference | users/operators |
| `docs/providers-reference.md` | Current Reference | users/operators |
| `docs/channels-reference.md` | Current Reference | users/operators |
| `docs/nextcloud-talk-setup.md` | Current Guide | operators |
| `docs/config-reference.md` | Current Reference | operators |
| `docs/custom-providers.md` | Current Integration Guide | integration developers |
| `docs/zai-glm-setup.md` | Current Provider Setup Guide | users/operators |
| `docs/langgraph-integration.md` | Current Integration Guide | integration developers |
| `docs/operations-runbook.md` | Current Guide | operators |
| `docs/troubleshooting.md` | Current Guide | users/operators |
| `docs/network-deployment.md` | Current Guide | operators |
| `docs/mattermost-setup.md` | Current Guide | operators |
| `docs/adding-boards-and-tools.md` | Current Guide | hardware builders |
| `docs/arduino-uno-q-setup.md` | Current Guide | hardware builders |
| `docs/nucleo-setup.md` | Current Guide | hardware builders |
| `docs/hardware-peripherals-design.md` | Current Design Spec | hardware contributors |
| `docs/datasheets/nucleo-f401re.md` | Current Hardware Reference | hardware builders |
| `docs/datasheets/arduino-uno.md` | Current Hardware Reference | hardware builders |
| `docs/datasheets/esp32.md` | Current Hardware Reference | hardware builders |

## Policy / Process Docs

| Doc | Type |
|---|---|
| `docs/pr-workflow.md` | Policy |
| `docs/reviewer-playbook.md` | Process |
| `docs/ci-map.md` | Process |
| `docs/actions-source-policy.md` | Policy |

## Proposal / Roadmap Docs

These are valuable context, but **not strict runtime contracts**.

| Doc | Type |
|---|---|
| `docs/sandboxing.md` | Proposal |
| `docs/resource-limits.md` | Proposal |
| `docs/audit-logging.md` | Proposal |
| `docs/agnostic-security.md` | Proposal |
| `docs/frictionless-security.md` | Proposal |
| `docs/security-roadmap.md` | Roadmap |

## Snapshot Docs

| Doc | Type |
|---|---|
| `docs/project-triage-snapshot-2026-02-18.md` | Snapshot |

## Maintenance Recommendations

1. Update `commands-reference` whenever CLI surface changes.
2. Update `providers-reference` when provider catalog/aliases/env vars change.
3. Update `channels-reference` when channel support or allowlist semantics change.
4. Keep snapshots date-stamped and immutable.
5. Mark proposal docs clearly to avoid being mistaken for runtime contracts.
6. Keep localized README/docs-hub links aligned when adding new core docs.
7. Update `docs/SUMMARY.md` and collection indexes whenever new major docs are added.
