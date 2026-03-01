# 在地化橋接檔案：Proxy Agent Playbook

這是增強型 bridge 頁面。它提供該主題的定位、原文章節導覽和執行提示，使用說明你在不丟失英文規範語義的情況下快速落地。

英文原文:

- [../../proxy-agent-playbook.md](../../proxy-agent-playbook.md)

## 主題定位

- 類別：Provider 與整合
- 深度：增強 bridge（章節導覽 + 執行提示）
- 適用：先理解結構，再按英文規範逐條執行。

## 原文章節導覽

- [H2 · 0. Summary](../../proxy-agent-playbook.md#0-summary)
- [H2 · 1. Fast Path by Intent](../../proxy-agent-playbook.md#1-fast-path-by-intent)
- [H3 · 1.1 Proxy only ZeroClaw internal traffic](../../proxy-agent-playbook.md#1-1-proxy-only-zeroclaw-internal-traffic)
- [H3 · 1.2 Proxy only selected services](../../proxy-agent-playbook.md#1-2-proxy-only-selected-services)
- [H3 · 1.3 Export process-wide proxy environment variables](../../proxy-agent-playbook.md#1-3-export-process-wide-proxy-environment-variables)
- [H3 · 1.4 Emergency rollback](../../proxy-agent-playbook.md#1-4-emergency-rollback)
- [H2 · 2. Scope Decision Matrix](../../proxy-agent-playbook.md#2-scope-decision-matrix)
- [H2 · 3. Standard Safe Workflow](../../proxy-agent-playbook.md#3-standard-safe-workflow)
- [H2 · 4. Mode A — Proxy Only for ZeroClaw Internals](../../proxy-agent-playbook.md#4-mode-a-proxy-only-for-zeroclaw-internals)
- [H2 · 5. Mode B — Proxy Only for Specific Services](../../proxy-agent-playbook.md#5-mode-b-proxy-only-for-specific-services)
- [H3 · 5.1 Target specific services](../../proxy-agent-playbook.md#5-1-target-specific-services)
- [H3 · 5.2 Target by selectors](../../proxy-agent-playbook.md#5-2-target-by-selectors)
- [H2 · 6. Mode C — Proxy for Full Process Environment](../../proxy-agent-playbook.md#6-mode-c-proxy-for-full-process-environment)
- [H3 · 6.1 Configure and apply environment scope](../../proxy-agent-playbook.md#6-1-configure-and-apply-environment-scope)
- [H2 · 7. Disable / Rollback Patterns](../../proxy-agent-playbook.md#7-disable-rollback-patterns)
- [H3 · 7.1 Disable proxy (default safe behavior)](../../proxy-agent-playbook.md#7-1-disable-proxy-default-safe-behavior)
- [H3 · 7.2 Disable proxy and force-clear env vars](../../proxy-agent-playbook.md#7-2-disable-proxy-and-force-clear-env-vars)
- [H3 · 7.3 Keep proxy enabled but clear environment exports only](../../proxy-agent-playbook.md#7-3-keep-proxy-enabled-but-clear-environment-exports-only)

## 操作建議

- 先通讀原文目錄，再聚焦與你當前變更直接相關的小節。
- 指令名、配置鍵、API 路徑和程式碼標識保持英文。
- 發生語義歧義或行為衝突時，以英文原文為準。

## 相關入口

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
