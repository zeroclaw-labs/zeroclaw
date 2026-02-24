# ZeroClaw Documentation Inventory

This inventory classifies documentation by intent and canonical location.

Last reviewed: **February 24, 2026**.

## Classification Legend

- **Current Guide/Reference**: intended to match current runtime behavior
- **Policy/Process**: contribution or governance contract
- **Proposal/Roadmap**: exploratory or planned behavior
- **Snapshot/Audit**: time-bound status and gap analysis
- **Compatibility Shim**: path preserved for backward navigation

## Entry Points

### Product root

| Doc | Type | Audience |
|---|---|---|
| `README.md` | Current Guide | all readers |
| `README.zh-CN.md` | Current Guide (localized) | Chinese readers |
| `README.ja.md` | Current Guide (localized) | Japanese readers |
| `README.ru.md` | Current Guide (localized) | Russian readers |
| `README.fr.md` | Current Guide (localized) | French readers |
| `README.vi.md` | Current Guide (localized) | Vietnamese readers |
| `README.el.md` | Current Guide (localized) | Greek readers |

### Docs system

| Doc | Type | Audience |
|---|---|---|
| `docs/README.md` | Current Guide (hub) | all readers |
| `docs/SUMMARY.md` | Current Guide (unified TOC) | all readers |
| `docs/structure/README.md` | Current Guide (structure map) | maintainers |
| `docs/i18n-guide.md` | Current Guide (i18n completion contract) | contributors/agents |
| `docs/i18n/README.md` | Current Guide (locale index) | maintainers/translators |
| `docs/i18n-coverage.md` | Current Guide (coverage matrix) | maintainers/translators |

## Locale Hubs (Canonical)

| Locale | Canonical hub | Type |
|---|---|---|
| `zh-CN` | `docs/i18n/zh-CN/README.md` | Current Guide (localized hub scaffold) |
| `ja` | `docs/i18n/ja/README.md` | Current Guide (localized hub scaffold) |
| `ru` | `docs/i18n/ru/README.md` | Current Guide (localized hub scaffold) |
| `fr` | `docs/i18n/fr/README.md` | Current Guide (localized hub scaffold) |
| `vi` | `docs/i18n/vi/README.md` | Current Guide (full localized tree) |
| `el` | `docs/i18n/el/README.md` | Current Guide (full localized tree) |

Compatibility shims such as `docs/README.<locale>.md`, `docs/SUMMARY.<locale>.md`, and `docs/vi/**` remain valid but are non-canonical.

## Collection Index Docs (English canonical)

| Doc | Type | Audience |
|---|---|---|
| `docs/getting-started/README.md` | Current Guide | new users |
| `docs/reference/README.md` | Current Guide | users/operators |
| `docs/operations/README.md` | Current Guide | operators |
| `docs/security/README.md` | Current Guide | operators/contributors |
| `docs/hardware/README.md` | Current Guide | hardware builders |
| `docs/contributing/README.md` | Current Guide | contributors/reviewers |
| `docs/project/README.md` | Current Guide | maintainers |
| `docs/sop/README.md` | Current Guide | operators/automation maintainers |

## Current Guides & References

| Doc | Type | Audience |
|---|---|---|
| `docs/one-click-bootstrap.md` | Current Guide | users/operators |
| `docs/android-setup.md` | Current Guide | Android users/operators |
| `docs/commands-reference.md` | Current Reference | users/operators |
| `docs/providers-reference.md` | Current Reference | users/operators |
| `docs/channels-reference.md` | Current Reference | users/operators |
| `docs/config-reference.md` | Current Reference | operators |
| `docs/custom-providers.md` | Current Integration Guide | integration developers |
| `docs/zai-glm-setup.md` | Current Provider Setup Guide | users/operators |
| `docs/langgraph-integration.md` | Current Integration Guide | integration developers |
| `docs/proxy-agent-playbook.md` | Current Operations Playbook | operators/maintainers |
| `docs/operations-runbook.md` | Current Guide | operators |
| `docs/operations/connectivity-probes-runbook.md` | Current CI/ops Runbook | maintainers/operators |
| `docs/troubleshooting.md` | Current Guide | users/operators |
| `docs/network-deployment.md` | Current Guide | operators |
| `docs/mattermost-setup.md` | Current Guide | operators |
| `docs/nextcloud-talk-setup.md` | Current Guide | operators |
| `docs/cargo-slicer-speedup.md` | Current Build/CI Guide | maintainers |
| `docs/adding-boards-and-tools.md` | Current Guide | hardware builders |
| `docs/arduino-uno-q-setup.md` | Current Guide | hardware builders |
| `docs/nucleo-setup.md` | Current Guide | hardware builders |
| `docs/hardware-peripherals-design.md` | Current Design Spec | hardware contributors |
| `docs/datasheets/README.md` | Current Hardware Index | hardware builders |
| `docs/datasheets/nucleo-f401re.md` | Current Hardware Reference | hardware builders |
| `docs/datasheets/arduino-uno.md` | Current Hardware Reference | hardware builders |
| `docs/datasheets/esp32.md` | Current Hardware Reference | hardware builders |
| `docs/audit-event-schema.md` | Current CI/Security Reference | maintainers/security reviewers |

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

## Snapshot / Audit Docs

| Doc | Type |
|---|---|
| `docs/project-triage-snapshot-2026-02-18.md` | Snapshot |
| `docs/docs-audit-2026-02-24.md` | Snapshot (docs architecture audit) |
| `docs/i18n-gap-backlog.md` | Snapshot (i18n depth gap tracking) |

## Maintenance Contract

1. Update `docs/SUMMARY.md` and nearest category index when adding a major doc.
2. Keep locale navigation parity across all supported locales (`en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`, `el`).
3. Use `docs/i18n-guide.md` whenever docs IA/shared wording changes.
4. Keep canonical localized hubs under `docs/i18n/<locale>/`; treat shim paths as compatibility only.
5. Keep snapshots date-stamped and immutable; add newer snapshots instead of rewriting historical ones.
