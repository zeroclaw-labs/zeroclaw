---
type: reference
status: active
last-reviewed: 2026-01-01
relates-to:
  - docs/proposals/documentation-standards.md
---

# Documentation Migration Manifest

Migration checklist for the documentation restructure described in
[RFC #5576 — Intentional Documentation](../proposals/documentation-standards.md).

Every document from the previous `docs/` tree is listed here with its
classification and target disposition. Check each row off as items land
in their destination.

---

## Classification Schema

### Artifact Family (RFC §3)

| Family | The Question It Answers |
|---|---|
| **Consideration** | What principles and standards guide our decisions? |
| **Landscape** | What does the system look like right now? |
| **Outline** | Where are we going? |
| **Design** | How exactly are we doing this specific thing? |
| **Standard** | What are the specific rules for how we build? |
| **Operational** | How do users set up, operate, or troubleshoot the system? |

### Destination

| Value | Meaning |
|---|---|
| `repo:[path]` | Promoted to the given path in the new `docs/` structure |
| `wiki:[section]` | Migrated to the GitHub Wiki under the given section |
| `delete` | Obsolete, superseded, or replaced; safe to remove |

### Freshness

| Value | Meaning |
|---|---|
| `current` | Verified accurate against the current codebase |
| `stale` | Partially outdated; needs targeted updates before promotion |
| `obsolete` | Describes something that no longer exists or has been fully superseded |
| `proposal` | Describes future or aspirational state written as if current — highest risk |

### Priority

| Value | Meaning |
|---|---|
| `must-have` | Promotion or migration is a blocker for Phase 2–3 completion |
| `nice-to-have` | Valuable but Phase 2–3 can close without it |
| `not-needed` | Will not be promoted or migrated; targeted for deletion |

---

## i18n Directory

The entire `docs/i18n/` tree (169 files, ~2.2 MB, 30 locale subdirectories)
is removed as part of Phase 1. RFC #5576 §4 documents the rationale.
Community translations move to the GitHub Wiki under a Translations page
maintained by volunteer coordinators, with no parity requirement.

| - | Item | Disposition | Notes |
|---|---|---|---|
| `[ ]` | `docs/i18n/` (entire directory, 30 locales) | `delete` | i18n infrastructure removed per RFC §4. Wiki Translations page replaces. |

---

## Architecture

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/architecture/adr-004-tool-shared-state-ownership.md` | Design | `repo:docs/architecture/decisions/ADR-004-tool-shared-state-ownership.md` | current | must-have | Rename to match ADR numbering convention. Only existing ADR. |
| `[ ]` | `docs/assets/architecture-diagrams.md` | Landscape | `repo:docs/architecture/diagrams/` | stale | nice-to-have | Update for post-#5559 crate topology before promotion. Convert to Mermaid if not already. |

---

## Contributing

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/contributing/README.md` | Standard | `repo:docs/contributing/README.md` | stale | must-have | Update index links to reflect new structure. |
| `[ ]` | `docs/contributing/actions-source-policy.md` | Standard | `repo:docs/contributing/actions-source-policy.md` | current | must-have | Verify still accurate against current workflow pins. |
| `[ ]` | `docs/contributing/adding-boards-and-tools.md` | Standard | `repo:docs/contributing/adding-boards-and-tools.md` | stale | must-have | Verify against current peripheral trait surface post-#5559. |
| `[ ]` | `docs/contributing/cargo-slicer-speedup.md` | Standard | `repo:docs/contributing/cargo-slicer-speedup.md` | stale | nice-to-have | Verify still applies to workspace structure post-#5559. |
| `[ ]` | `docs/contributing/change-playbooks.md` | Standard | `repo:docs/contributing/change-playbooks.md` | stale | must-have | Verify playbook steps against new crate boundaries. |
| `[ ]` | `docs/contributing/ci-map.md` | Standard | `repo:docs/contributing/ci-map.md` | stale | must-have | Verify against current `.github/workflows/`. Will need update when CI/CD RFC (#5579) lands. |
| `[ ]` | `docs/contributing/cla.md` | Standard | `repo:docs/contributing/cla.md` | current | must-have | Legal/process — unlikely to be stale. Spot-check. |
| `[ ]` | `docs/contributing/custom-providers.md` | Standard | `repo:docs/contributing/custom-providers.md` | stale | must-have | Verify trait signatures and module paths against post-#5559 codebase. |
| `[ ]` | `docs/contributing/doc-template.md` | Standard | `repo:docs/contributing/doc-template.md` | stale | must-have | Update to reflect new classification schema and YAML frontmatter requirement. |
| `[ ]` | `docs/contributing/docs-contract.md` | Standard | `repo:docs/contributing/docs-contract.md` | obsolete | must-have | Replaced by new docs-contract per RFC §9. Rewrite in place rather than promote as-is. |
| `[ ]` | `docs/contributing/extension-examples.md` | Standard | `repo:docs/contributing/extension-examples.md` | stale | nice-to-have | Verify code examples compile against post-#5559 trait surface. |
| `[ ]` | `docs/contributing/label-registry.md` | Standard | `repo:docs/contributing/label-registry.md` | current | must-have | Spot-check labels against current GitHub label set. |
| `[ ]` | `docs/contributing/pr-discipline.md` | Standard | `repo:docs/contributing/pr-discipline.md` | current | must-have | Privacy and attribution rules — unlikely to be stale. |
| `[ ]` | `docs/contributing/pr-workflow.md` | Standard | `repo:docs/contributing/pr-workflow.md` | stale | must-have | Update for new branch/crate structure. |
| `[ ]` | `docs/contributing/release-process.md` | Standard | `repo:docs/contributing/release-process.md` | stale | must-have | Update when CI/CD RFC (#5579) Phase 1 lands. |
| `[ ]` | `docs/contributing/reviewer-playbook.md` | Standard | `repo:docs/contributing/reviewer-playbook.md` | stale | must-have | Add architecture-review section per Governance RFC (#5577). |
| `[ ]` | `docs/contributing/testing.md` | Standard | `repo:docs/contributing/testing.md` | stale | must-have | Verify test commands against workspace structure post-#5559. |
| `[ ]` | `docs/contributing/testing-telegram.md` | Standard | `repo:docs/contributing/testing-telegram.md` | stale | nice-to-have | Verify setup steps still accurate. Consider moving to wiki if purely operational. |

---

## Getting Started

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/getting-started/multi-model-setup.md` | Operational | `wiki:Getting Started/Multi-Model Setup` | stale | must-have | Verify config syntax against current provider config structs. |

---

## Hardware

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/hardware/README.md` | Standard | `repo:docs/hardware/README.md` | stale | must-have | Update index links. |
| `[ ]` | `docs/hardware/hardware-peripherals-design.md` | Design | `repo:docs/hardware/hardware-peripherals-design.md` | stale | must-have | Verify against peripheral trait surface post-#5559. |
| `[ ]` | `docs/hardware/datasheets/arduino-uno.md` | Design | `repo:docs/hardware/datasheets/arduino-uno.md` | current | nice-to-have | Hardware spec — unlikely to be stale. Spot-check. |
| `[ ]` | `docs/hardware/datasheets/esp32.md` | Design | `repo:docs/hardware/datasheets/esp32.md` | current | nice-to-have | Hardware spec — unlikely to be stale. Spot-check. |
| `[ ]` | `docs/hardware/datasheets/nucleo-f401re.md` | Design | `repo:docs/hardware/datasheets/nucleo-f401re.md` | current | nice-to-have | Hardware spec — unlikely to be stale. Spot-check. |
| `[ ]` | `docs/hardware/android-setup.md` | Operational | `wiki:Hardware/Android Setup` | stale | nice-to-have | Verify setup steps. |
| `[ ]` | `docs/hardware/arduino-uno-q-setup.md` | Operational | `wiki:Hardware/Arduino Uno Q Setup` | stale | nice-to-have | Verify setup steps. |
| `[ ]` | `docs/hardware/nucleo-setup.md` | Operational | `wiki:Hardware/STM32 Nucleo Setup` | stale | nice-to-have | Verify setup steps. |

---

## Maintainers

The `docs/maintainers/` tree is operational and snapshot content.
Most items move to the Wiki. Two items are code-adjacent enough to promote to `docs/architecture/`.

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/maintainers/README.md` | Standard | `wiki:Maintainers/Overview` | stale | nice-to-have | Hub page for maintainer content on wiki. |
| `[ ]` | `docs/maintainers/docs-inventory.md` | Landscape | `delete` | obsolete | not-needed | Superseded by this MANIFEST. |
| `[ ]` | `docs/maintainers/project-triage-snapshot-2026-02-18.md` | Snapshot | `wiki:Maintainers/Triage Snapshots` | current | nice-to-have | Time-bound snapshot; immutable. Move to wiki for historical reference. |
| `[ ]` | `docs/maintainers/refactor-candidates.md` | Landscape | `wiki:Maintainers/Refactor Candidates` | stale | nice-to-have | Living list; update after #5559 lands. Consider whether it belongs in repo as an Outline. |
| `[ ]` | `docs/maintainers/repo-map.md` | Landscape | `repo:docs/architecture/repo-map.md` | stale | must-have | Code-adjacent system map. Update for post-#5559 crate topology. |
| `[ ]` | `docs/maintainers/structure-README.md` | Landscape | `repo:docs/architecture/structure.md` | stale | must-have | Describes repo structure — code-adjacent, needs update for new layout. |
| `[ ]` | `docs/maintainers/trademark.md` | Consideration | `repo:docs/contributing/trademark.md` | current | must-have | Legal/governance — code-adjacent enough to stay in repo. |

---

## Ops

All `docs/ops/` content is operational and moves to the GitHub Wiki.

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/ops/README.md` | Operational | `wiki:Operations/Overview` | stale | must-have | Hub page for operations section. |
| `[ ]` | `docs/ops/operations-runbook.md` | Operational | `wiki:Operations/Runbook` | stale | must-have | High-value operational content. Verify procedures still accurate. |
| `[ ]` | `docs/ops/troubleshooting.md` | Operational | `wiki:Operations/Troubleshooting` | stale | must-have | High-value for users. Verify error messages and steps. |
| `[ ]` | `docs/ops/network-deployment.md` | Operational | `wiki:Operations/Network Deployment` | stale | must-have | Verify deployment steps. |
| `[ ]` | `docs/ops/proxy-agent-playbook.md` | Operational | `wiki:Operations/Proxy Agent Playbook` | stale | nice-to-have | Verify still accurate. |
| `[ ]` | `docs/ops/resource-limits.md` | Operational | `wiki:Operations/Resource Limits` | stale | nice-to-have | Verify config values still match current defaults. |

---

## Proposals (RFCs)

All four RFCs stay in the repository. They are Outline artifacts and version with the codebase.

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/proposals/microkernel-architecture.md` | Outline | `repo:docs/proposals/microkernel-architecture.md` | current | must-have | Status: proposed. Linked to #5574 and PR #5559. No move, no edit needed in Phase 1. |
| `[ ]` | `docs/proposals/documentation-standards.md` | Outline | `repo:docs/proposals/documentation-standards.md` | current | must-have | This RFC. Status: proposed. Linked to #5576. |
| `[ ]` | `docs/proposals/project-governance.md` | Outline | `repo:docs/proposals/project-governance.md` | current | must-have | Status: proposed. Linked to #5577. |
| `[ ]` | `docs/proposals/ci-pipeline.md` | Outline | `repo:docs/proposals/ci-pipeline.md` | current | must-have | Status: proposed. Linked to #5579. |

---

## Reference

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/reference/README.md` | Standard | `repo:docs/reference/README.md` | stale | must-have | Update index links. |
| `[ ]` | `docs/reference/api/config-reference.md` | Design | `repo:docs/reference/api/config-reference.md` | stale | must-have | Audit config keys against actual config structs in source. High audit priority. |
| `[ ]` | `docs/reference/api/providers-reference.md` | Design | `repo:docs/reference/api/providers-reference.md` | stale | must-have | Audit provider names, aliases, env vars against source. High audit priority. |
| `[ ]` | `docs/reference/api/channels-reference.md` | Design | `repo:docs/reference/api/channels-reference.md` | stale | must-have | Audit channel names, config keys against source. High audit priority. |
| `[ ]` | `docs/reference/cli/commands-reference.md` | Design | `repo:docs/reference/cli/commands-reference.md` | stale | must-have | Audit CLI flags and subcommands against `src/main.rs` clap definitions. High audit priority. |
| `[ ]` | `docs/reference/sop/README.md` | Standard | `repo:docs/reference/sop/README.md` | stale | nice-to-have | Assess whether SOP section belongs in repo or wiki. |
| `[ ]` | `docs/reference/sop/syntax.md` | Design | `repo:docs/reference/sop/syntax.md` | stale | must-have | Syntax reference is code-adjacent. Audit against current parser behavior. |
| `[ ]` | `docs/reference/sop/observability.md` | Design | `repo:docs/reference/sop/observability.md` | stale | must-have | Observability config is code-adjacent. Audit against current instrumentation. |
| `[ ]` | `docs/reference/sop/connectivity.md` | Standard | `repo:docs/reference/sop/connectivity.md` | stale | nice-to-have | Assess: code-adjacent reference or operational guide? |
| `[ ]` | `docs/reference/sop/cookbook.md` | Operational | `wiki:Reference/Cookbook` | stale | nice-to-have | Usage patterns belong on wiki. |

---

## Security

Security docs split: policy and design stay in repo, operational guides move to wiki.

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/security/README.md` | Standard | `repo:docs/security/README.md` | stale | must-have | Update index links. |
| `[ ]` | `docs/security/agnostic-security.md` | Consideration | `repo:docs/security/agnostic-security.md` | proposal | must-have | Describes aspirational security posture. Label frontmatter `status: proposal` before promotion. |
| `[ ]` | `docs/security/frictionless-security.md` | Consideration | `repo:docs/security/frictionless-security.md` | proposal | must-have | Same as above. Clearly a proposal, not current behavior. |
| `[ ]` | `docs/security/sandboxing.md` | Design | `repo:docs/security/sandboxing.md` | proposal | must-have | Describes proposed sandbox model. Label `status: proposal`. Verify what is actually implemented. |
| `[ ]` | `docs/security/audit-logging.md` | Design | `repo:docs/security/audit-logging.md` | proposal | must-have | Describes proposed audit logging. Label `status: proposal`. Verify what is actually implemented. |
| `[ ]` | `docs/security/security-roadmap.md` | Outline | `repo:docs/security/security-roadmap.md` | stale | must-have | Update roadmap milestones against actual shipped state. |
| `[ ]` | `docs/security/matrix-e2ee-guide.md` | Operational | `wiki:Security/Matrix E2EE Guide` | stale | nice-to-have | Setup guide for a specific channel — operational, not code-adjacent. |

---

## Setup Guides

All `docs/setup-guides/` content moves to the GitHub Wiki.

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/setup-guides/README.md` | Operational | `wiki:Setup Guides/Overview` | stale | must-have | Hub page. |
| `[ ]` | `docs/setup-guides/one-click-bootstrap.md` | Operational | `wiki:Getting Started/One-Click Bootstrap` | stale | must-have | High-traffic entry point. Verify all steps. |
| `[ ]` | `docs/setup-guides/windows-setup.md` | Operational | `wiki:Getting Started/Windows Setup` | stale | must-have | Verify steps against current install.bat / setup.bat. |
| `[ ]` | `docs/setup-guides/macos-update-uninstall.md` | Operational | `wiki:Getting Started/macOS Update and Uninstall` | stale | must-have | Verify steps. |
| `[ ]` | `docs/setup-guides/mattermost-setup.md` | Operational | `wiki:Channels/Mattermost` | stale | must-have | Verify against current Mattermost channel config. |
| `[ ]` | `docs/setup-guides/mcp-setup.md` | Operational | `wiki:Setup Guides/MCP Setup` | stale | nice-to-have | Verify steps. |
| `[ ]` | `docs/setup-guides/nextcloud-talk-setup.md` | Operational | `wiki:Channels/Nextcloud Talk` | stale | nice-to-have | Verify against current channel config. |
| `[ ]` | `docs/setup-guides/zai-glm-setup.md` | Operational | `wiki:Channels/ZAI GLM` | stale | nice-to-have | Verify against current provider config. |

---

## Superpowers Specs

Two early design specs that were not present in the docs inventory and are not yet classified.
Both are time-stamped 2026, suggesting recent authorship. Assessment required before disposition.

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/superpowers/specs/2026-03-13-linkedin-tool-design.md` | Outline | `repo:docs/proposals/superpowers/linkedin-tool-design.md` | current | nice-to-have | Likely an Outline (tool design spec). Confirm with author. Move to proposals/ if so. |
| `[ ]` | `docs/superpowers/specs/2026-03-19-google-workspace-operation-allowlist.md` | Outline | `repo:docs/proposals/superpowers/google-workspace-operation-allowlist.md` | current | nice-to-have | Same as above. Confirm with author before moving. |

---

## Top-Level Loose Files

Files at `docs/` root that were not in the original inventory and need individual assessment.

| - | Original Path | Family | Destination | Freshness | Priority | Notes |
|---|---|---|---|---|---|---|
| `[ ]` | `docs/README.md` | Standard | `repo:docs/README.md` | stale | must-have | Hub page. Update links to reflect new structure and wiki. |
| `[ ]` | `docs/SUMMARY.md` | Standard | `repo:docs/SUMMARY.md` | stale | must-have | Canonical TOC. Rebuild to reflect new structure (repo docs only, no wiki links). |
| `[ ]` | `docs/aardvark-integration.md` | Operational | `wiki:Integrations/Aardvark` | stale | nice-to-have | Integration guide — operational, not code-adjacent. Verify steps. |
| `[ ]` | `docs/browser-setup.md` | Operational | `wiki:Setup Guides/Browser Setup` | stale | nice-to-have | Setup guide — operational. Verify steps. |
| `[ ]` | `docs/openai-temperature-compatibility.md` | Design | `repo:docs/reference/api/openai-temperature-compatibility.md` | stale | nice-to-have | Provider compatibility note — code-adjacent. Audit against current provider implementation. |

---

## Missing Artifacts (to be created in Phase 2–3)

These artifacts do not yet exist but are called for by the Architecture RFC (#5574)
or the Documentation RFC (#5576). They are tracked here so Phase 2–3 has a complete
work list.

| Item | Family | Target Path | Source RFC | Notes |
|---|---|---|---|---|
| ADR-001: Rust as the implementation language | Design | `docs/architecture/decisions/ADR-001-rust-first.md` | #5574, #5576 | Retroactive. Core decision never recorded. |
| ADR-002: Trait-driven extensibility model | Design | `docs/architecture/decisions/ADR-002-trait-driven-extensibility.md` | #5574 | Retroactive. The fundamental architecture decision. |
| ADR-003: WASM plugin model | Design | `docs/architecture/decisions/ADR-003-wasm-plugin-model.md` | #5574 | Retroactive. Document current intent and groundwork. |
| ADR-005: Memory backends (SQLite + Markdown) | Design | `docs/architecture/decisions/ADR-005-memory-backends.md` | #5574 | Retroactive. |
| ADR-006: CLI as the only built-in channel | Design | `docs/architecture/decisions/ADR-006-cli-only-built-in-channel.md` | #5574 | Retroactive. |
| ADR-007: Gateway extraction | Design | `docs/architecture/decisions/ADR-007-gateway-extraction.md` | #5574 | Retroactive. |
| Component map (Mermaid) | Landscape | `docs/architecture/diagrams/component-map.md` | #5576 | Depends on #5559 landing — draw against actual crate topology. |
| Data flow diagram (Mermaid) | Landscape | `docs/architecture/diagrams/data-flow.md` | #5576 | Message lifecycle through the new crate structure. |
| Per-crate `AGENTS.md` files | Consideration | `crates/<name>/AGENTS.md` | #5576 §7 | One per new crate from #5559. Priority: `zeroclaw-api` first. |
| Plugin SDK documentation | Standard | `docs/contributing/plugin-sdk.md` | #5574, #5576 | Depends on WIT interface files landing (Phase 3). |
| New `docs-contract.md` | Standard | `docs/contributing/docs-contract.md` | #5576 §9 | Replaces current stale version. Full text specified in RFC §9. |

---

## Progress Summary

> Update these counts as items are checked off.

| Destination | Total | Completed |
|---|---|---|
| `repo:` | 46 | 0 |
| `wiki:` | 27 | 0 |
| `delete` | 2 | 0 |
| **Total** | **75** | **0** |