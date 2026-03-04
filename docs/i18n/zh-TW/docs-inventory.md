# ZeroClaw 文件清單（繁體中文）

本清單依據用途與正式路徑分類所有文件。

最近審閱日期：**2026 年 2 月 24 日**。

## 分類說明

- **現行指南/參考**：對應當前執行期行為的文件
- **政策/流程**：貢獻與治理規範
- **提案/路線圖**：探索性或規劃中的功能
- **快照/稽核**：特定時間點的狀態與缺口分析
- **相容性墊片**：為維持向下相容導航而保留的路徑

## 進入點

### 產品根目錄

| 文件 | 類型 | 讀者 |
|---|---|---|
| `README.md` | 現行指南 | 所有讀者 |
| `docs/i18n/zh-CN/README.md` | 現行指南（在地化） | 簡體中文讀者 |
| `docs/i18n/ja/README.md` | 現行指南（在地化） | 日文讀者 |
| `docs/i18n/ru/README.md` | 現行指南（在地化） | 俄文讀者 |
| `docs/i18n/fr/README.md` | 現行指南（在地化） | 法文讀者 |
| `docs/i18n/vi/README.md` | 現行指南（在地化） | 越南文讀者 |
| `docs/i18n/el/README.md` | 現行指南（在地化） | 希臘文讀者 |

### 文件系統

| 文件 | 類型 | 讀者 |
|---|---|---|
| `docs/README.md` | 現行指南（中心頁） | 所有讀者 |
| `docs/SUMMARY.md` | 現行指南（統一目錄） | 所有讀者 |
| `docs/structure/README.md` | 現行指南（結構地圖） | 維護者 |
| `docs/structure/by-function.md` | 現行指南（功能地圖） | 維護者/運維人員 |
| `docs/i18n-guide.md` | 現行指南（i18n 完成規範） | 貢獻者/代理程式 |
| `docs/i18n/README.md` | 現行指南（語系索引） | 維護者/翻譯者 |
| `docs/i18n-coverage.md` | 現行指南（覆蓋率矩陣） | 維護者/翻譯者 |

## 語系中心頁（正式路徑）

| 語系 | 正式中心頁 | 類型 |
|---|---|---|
| `zh-CN` | `docs/i18n/zh-CN/README.md` | 現行指南（在地化中心頁骨架） |
| `ja` | `docs/i18n/ja/README.md` | 現行指南（在地化中心頁骨架） |
| `ru` | `docs/i18n/ru/README.md` | 現行指南（在地化中心頁骨架） |
| `fr` | `docs/i18n/fr/README.md` | 現行指南（在地化中心頁骨架） |
| `vi` | `docs/i18n/vi/README.md` | 現行指南（完整在地化樹） |
| `el` | `docs/i18n/el/README.md` | 現行指南（完整在地化樹） |

`docs/SUMMARY.<locale>.md` 與 `docs/vi/**` 等相容性墊片仍然有效，但並非正式路徑。

## 分類索引文件（英文正式版）

| 文件 | 類型 | 讀者 |
|---|---|---|
| `docs/getting-started/README.md` | 現行指南 | 新使用者 |
| `docs/reference/README.md` | 現行指南 | 使用者/運維人員 |
| `docs/operations/README.md` | 現行指南 | 運維人員 |
| `docs/security/README.md` | 現行指南 | 運維人員/貢獻者 |
| `docs/hardware/README.md` | 現行指南 | 硬體開發者 |
| `docs/contributing/README.md` | 現行指南 | 貢獻者/審查者 |
| `docs/project/README.md` | 現行指南 | 維護者 |
| `docs/sop/README.md` | 現行指南 | 運維人員/自動化維護者 |

## 現行指南與參考文件

| 文件 | 類型 | 讀者 |
|---|---|---|
| `docs/one-click-bootstrap.md` | 現行指南 | 使用者/運維人員 |
| `docs/android-setup.md` | 現行指南 | Android 使用者/運維人員 |
| `docs/commands-reference.md` | 現行參考 | 使用者/運維人員 |
| `docs/providers-reference.md` | 現行參考 | 使用者/運維人員 |
| `docs/channels-reference.md` | 現行參考 | 使用者/運維人員 |
| `docs/config-reference.md` | 現行參考 | 運維人員 |
| `docs/custom-providers.md` | 現行整合指南 | 整合開發者 |
| `docs/zai-glm-setup.md` | 現行 Provider 設定指南 | 使用者/運維人員 |
| `docs/langgraph-integration.md` | 現行整合指南 | 整合開發者 |
| `docs/proxy-agent-playbook.md` | 現行維運手冊 | 運維人員/維護者 |
| `docs/operations-runbook.md` | 現行指南 | 運維人員 |
| `docs/operations/connectivity-probes-runbook.md` | 現行 CI/運維手冊 | 維護者/運維人員 |
| `docs/troubleshooting.md` | 現行指南 | 使用者/運維人員 |
| `docs/network-deployment.md` | 現行指南 | 運維人員 |
| `docs/mattermost-setup.md` | 現行指南 | 運維人員 |
| `docs/nextcloud-talk-setup.md` | 現行指南 | 運維人員 |
| `docs/cargo-slicer-speedup.md` | 現行建置/CI 指南 | 維護者 |
| `docs/adding-boards-and-tools.md` | 現行指南 | 硬體開發者 |
| `docs/arduino-uno-q-setup.md` | 現行指南 | 硬體開發者 |
| `docs/nucleo-setup.md` | 現行指南 | 硬體開發者 |
| `docs/hardware-peripherals-design.md` | 現行設計規格 | 硬體貢獻者 |
| `docs/datasheets/README.md` | 現行硬體索引 | 硬體開發者 |
| `docs/datasheets/nucleo-f401re.md` | 現行硬體參考 | 硬體開發者 |
| `docs/datasheets/arduino-uno.md` | 現行硬體參考 | 硬體開發者 |
| `docs/datasheets/esp32.md` | 現行硬體參考 | 硬體開發者 |
| `docs/audit-event-schema.md` | 現行 CI/安全參考 | 維護者/安全審查者 |
| `docs/security/official-channels-and-fraud-prevention.md` | 現行安全指南 | 使用者/運維人員 |

## 政策/流程文件

| 文件 | 類型 |
|---|---|
| `docs/pr-workflow.md` | 政策 |
| `docs/reviewer-playbook.md` | 流程 |
| `docs/ci-map.md` | 流程 |
| `docs/actions-source-policy.md` | 政策 |

## 提案/路線圖文件

這些文件提供有價值的脈絡，但**並非嚴格的執行期規範**。

| 文件 | 類型 |
|---|---|
| `docs/sandboxing.md` | 提案 |
| `docs/resource-limits.md` | 提案 |
| `docs/audit-logging.md` | 提案 |
| `docs/agnostic-security.md` | 提案 |
| `docs/frictionless-security.md` | 提案 |
| `docs/security-roadmap.md` | 路線圖 |

## 快照/稽核文件

| 文件 | 類型 |
|---|---|
| `docs/project-triage-snapshot-2026-02-18.md` | 快照 |
| `docs/docs-audit-2026-02-24.md` | 快照（文件架構稽核） |
| `docs/i18n-gap-backlog.md` | 快照（i18n 深度缺口追蹤） |

## 維護規範

1. 新增重要文件時，更新 `docs/SUMMARY.md` 與最近的分類索引。
2. 在所有支援語系（`en`、`zh-CN`、`ja`、`ru`、`fr`、`vi`、`el`）之間保持語系導航同等性。
3. 當文件資訊架構或共用措辭變更時，使用 `docs/i18n-guide.md` 進行檢查。
4. 將正式在地化中心頁放在 `docs/i18n/<locale>/` 下；墊片路徑僅作為相容性用途。
5. 快照文件應加註日期並保持不可變；建立新快照取代改寫歷史快照。
