# CI/Security Audit Event Schema

This document defines the normalized audit event envelope used by CI/CD and security workflows.

## Envelope

All audit events emitted by `scripts/ci/emit_audit_event.py` follow this top-level schema:

```json
{
  "schema_version": "zeroclaw.audit.v1",
  "event_type": "string",
  "generated_at": "RFC3339 timestamp",
  "run_context": {
    "repository": "owner/repo",
    "workflow": "workflow name",
    "run_id": "GitHub run id",
    "run_attempt": "GitHub run attempt",
    "sha": "commit sha",
    "ref": "git ref",
    "actor": "trigger actor"
  },
  "artifact": {
    "name": "artifact name",
    "retention_days": 14
  },
  "payload": {}
}
```

Notes:

- `artifact` is optional, but all CI/security audit lanes should populate it.
- `payload` preserves the original per-lane report JSON.

## Event Types

Current event types include:

- `ci_change_audit`
- `provider_connectivity`
- `reproducible_build`
- `supply_chain_provenance`
- `rollback_guard`
- `deny_policy_guard`
- `secrets_governance_guard`
- `gitleaks_scan`
- `sbom_snapshot`

## Retention Policy

Retention is encoded in workflow artifact uploads and mirrored into event metadata:

| Workflow | Artifact/Event | Retention |
| --- | --- | --- |
| `ci-change-audit.yml` | `ci-change-audit*` | 14 days |
| `ci-provider-connectivity.yml` | `provider-connectivity*` | 14 days |
| `ci-reproducible-build.yml` | `reproducible-build*` | 14 days |
| `sec-audit.yml` | deny/secrets/gitleaks/sbom artifacts | 14 days |
| `ci-rollback.yml` | `ci-rollback-plan*` | 21 days |
| `ci-supply-chain-provenance.yml` | `supply-chain-provenance` | 30 days |

## Governance

- Keep event payload schema stable and additive to avoid breaking downstream parsers.
- Use pinned actions and deterministic artifact naming for all audit lanes.
- Any retention policy change must be documented in this file and in `docs/ci-map.md`.
