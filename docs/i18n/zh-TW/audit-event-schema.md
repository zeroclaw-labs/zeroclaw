# CI/安全稽核事件架構（繁體中文）

本文件定義 CI/CD 和安全工作流程所使用的標準化稽核事件信封格式。

## 信封結構

所有由 `scripts/ci/emit_audit_event.py` 產生的稽核事件，皆遵循以下頂層架構：

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

注意事項：

- `artifact` 為選用欄位，但所有 CI/安全稽核流程都應填入此欄位。
- `payload` 保留各個流程原始的報告 JSON。

## 事件類型

目前的事件類型包括：

- `ci_change_audit`
- `provider_connectivity`
- `reproducible_build`
- `supply_chain_provenance`
- `rollback_guard`
- `deny_policy_guard`
- `secrets_governance_guard`
- `gitleaks_scan`
- `sbom_snapshot`

## 保留策略

保留期間編碼於工作流程的 artifact 上傳中，並同步至事件的中繼資料：

| 工作流程 | Artifact / 事件 | 保留期間 |
| --- | --- | --- |
| `ci-change-audit.yml` | `ci-change-audit*` | 14 天 |
| `ci-provider-connectivity.yml` | `provider-connectivity*` | 14 天 |
| `ci-reproducible-build.yml` | `reproducible-build*` | 14 天 |
| `sec-audit.yml` | deny/secrets/gitleaks/sbom artifacts | 14 天 |
| `ci-rollback.yml` | `ci-rollback-plan*` | 21 天 |
| `ci-supply-chain-provenance.yml` | `supply-chain-provenance` | 30 天 |

## 治理規範

- 事件酬載架構應保持穩定且僅做累加式變更，以避免破壞下游解析器。
- 所有稽核流程使用固定版本的 Actions 和確定性的 artifact 命名。
- 任何保留策略的變更都必須同時記錄在本檔案和 `docs/ci-map.md` 中。
