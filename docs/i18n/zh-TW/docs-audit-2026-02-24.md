# 文件稽核快照（2026-02-24）（繁體中文）

本快照記錄一次深度文件稽核，聚焦於完整性、導航清晰度與 i18n 結構。

日期：**2026-02-24**
範圍：專案文件（`docs/**`）+ 根目錄 README 語系進入點。

## 1) 稽核方法

- 對所有 markdown 文件執行結構性清點。
- 檢查文件目錄是否存在 README。
- 檢查所有文件 markdown 檔案的相對連結完整性。
- 審查正式路徑與相容性語系路徑的使用情形。
- 審查目錄/清單/結構地圖的一致性。

## 2) 發現事項

### A. 結構清晰度缺口

- 正式語系樹已存在於 `docs/i18n/<locale>/` 下，但部分治理文件仍描述舊版中心頁配置。
- `docs/vi/**` 相容性樹與 `docs/i18n/vi/**` 共存，造成維護上的模糊性。
- `datasheets` 目錄缺少明確的索引檔案（`README.md`），降低了可發現性。

### B. 完整性缺口

- 數份操作/參考文件未明確呈現在清單/摘要路徑中（例如 `audit-event-schema`、`proxy-agent-playbook`、`cargo-slicer-speedup`、`sop/*`、`operations/connectivity-probes-runbook`）。
- 語系覆蓋狀態已存在，但缺少明確的時間標記稽核快照來記錄目前的缺口與優先順序。

### C. 完整性問題

- 連結檢查發現壞掉的相對連結：
  - `docs/i18n/el/cargo-slicer-speedup.md` -> 工作流程路徑深度問題
  - `docs/vi/README.md` -> 相容性路徑中缺少 `SUMMARY.md`
  - `docs/vi/reference/README.md` -> 缺少 `../SUMMARY.md`

## 3) 已套用的修復

### 3.1 導航與治理

- 在先前階段已新增並連結 i18n 完成規範：`docs/i18n-guide.md`。
- 更新結構地圖，加入正式層級與相容性邊界：`docs/structure/README.md`。
- 更新清單，納入正式語系中心頁、SOP、CI/安全參考及稽核快照：`docs/docs-inventory.md`。

### 3.2 目錄完整性

新增缺少的 datasheet 索引：

- `docs/datasheets/README.md`
- `docs/i18n/vi/datasheets/README.md`
- `docs/i18n/el/datasheets/README.md`
- `docs/vi/datasheets/README.md`（相容性重導向）

### 3.3 相容性清理

- 將 `docs/vi/README.md` 轉換為明確的相容性中心頁，指向正式的 `docs/i18n/vi/**`。
- 將 `docs/vi/reference/README.md` 轉換為正式路徑重導向。

### 3.4 壞連結修復

- 修復希臘文 CI 工作流程的相對連結路徑。
- 透過重導向至正式路徑，消除相容性 README 的壞連結。

## 4) 目前已知的剩餘缺口

以下為結構/內容深度缺口，並非完整性問題：

1. 語系深度不對稱
- `vi`/`el` 擁有完整在地化樹。
- `zh-CN`/`ja`/`ru`/`fr` 目前提供中心頁層級骨架，而非完整的執行期規範在地化。

2. 相容性墊片生命週期
- `docs/vi/**` 仍為向下連結而存在；長期計畫應定義是保留還是完全棄用此映射。

3. 新治理文件的在地化傳播
- 新治理文件（例如本稽核快照及 i18n 指南）目前以英文優先流程撰寫；在地化摘要尚未完全傳播。

## 5) 建議的下一波次

1. 在 `docs/i18n/{zh-CN,ja,ru,fr}/` 下新增語系層級的迷你清單頁面，使中心頁骨架更具可操作性。
2. 定義並記錄 `docs/vi/**` 相容性路徑的正式棄用政策。
3. 在 CI 中新增輕量化的自動文件索引一致性檢查（摘要/清單交叉連結檢核）。

## 6) 驗證狀態

- 相對連結存在性檢查：修復後通過。
- `git diff --check`：乾淨。

本快照為 2026-02-24 文件重構工作的不可變脈絡。

## 附錄（Phase-2 深度完成）

在初始重構之後，同一日期範圍內套用了第二波完成作業：

- 新增在地化橋接覆蓋，使 `docs/i18n/vi/` 與 `docs/i18n/el/` 達到完整頂層文件同等（對照 `docs/*.md` 基準線）。
- 新增明確的 i18n 缺口待辦追蹤器：[i18n-gap-backlog.md](i18n-gap-backlog.md)。
- 在 `vi` 與 `el` 下新增 i18n 治理文件（`i18n-guide`、`i18n-coverage`）與最新文件稽核快照的在地化參考。
- 更新在地化中心頁與摘要（`docs/i18n/vi/*`、`docs/i18n/el/*`），揭露新增的文件與治理連結。

`zh-CN` / `ja` / `ru` / `fr` 的深度不對稱依設計維持（中心頁層級骨架），現已在待辦清單中明確追蹤數量與波次計畫。
