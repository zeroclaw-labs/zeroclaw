# ZeroClaw i18n 完成指南（繁體中文）

本指南定義當文件變更時，如何保持多語言文件的完整性與一致性。

## 適用範圍

當 PR 涉及任何使用者可見的文件導航、共用文件措辭、執行期規範參考或頂層文件治理時，請使用本指南。

主要文件表面：

- 根目錄首頁：`README.md`（語言切換連結指向 `docs/i18n/<locale>/README.md`）
- 文件中心頁：`docs/README.md`、`docs/i18n/<locale>/README.md`
- 統一目錄：`docs/SUMMARY.md`、`docs/SUMMARY.<locale>.md`（相容性墊片，若存在）
- i18n 索引與覆蓋率：`docs/i18n/README.md`、`docs/i18n-coverage.md`、`docs/i18n-gap-backlog.md`

支援語系：

- `en`（權威來源）
- `zh-CN`、`zh-TW`、`ja`、`ru`、`fr`、`vi`、`el`（在 `docs/i18n/<locale>/` 下完整頂層同等）

## 正式路徑配置

必要結構：

- 根目錄語言首頁：`README.md`
- 正式在地化文件中心頁：`docs/i18n/<locale>/README.md`
- 正式在地化摘要：`docs/i18n/<locale>/SUMMARY.md`
- 選用相容性墊片：`docs/SUMMARY.<locale>.md`（若為維持向下連結而保留）

相容性墊片可能存在於 docs 根目錄（例如 `docs/SUMMARY.zh-CN.md`），變更時必須保持同步。

## 觸發矩陣

使用此矩陣決定同一 PR 中需要的 i18n 跟進動作。

| 變更類型 | 必要的 i18n 跟進動作 |
|---|---|
| 根目錄 README 語言切換列變更 | 更新 `README.md` 中的語言切換列，並驗證所有在地化連結正確指向 `docs/i18n/<locale>/README.md` |
| 文件中心頁語言連結變更 | 更新 `docs/README.md` 及每個包含「其他語言」區段的 `docs/i18n/*/README.md` 中的在地化中心頁連結 |
| 統一目錄語言項目變更 | 更新 `docs/SUMMARY.md`、每個 `docs/i18n/*/SUMMARY.md`，以及 docs 根目錄的 `docs/SUMMARY*.md` 墊片（若存在） |
| 分類索引變更（`docs/<collection>/README.md`） | 更新 `docs/i18n/<locale>/<collection>/README.md` 下對應的在地化分類索引（針對維護在地化分類樹的語系） |
| `docs/*.md` 下的任何頂層執行期/治理/安全文件變更 | 在同一 PR 中更新每個 `docs/i18n/<locale>/` 下的對應檔案 |
| 語系新增/移除/重新命名 | 更新 `README.md`、文件中心頁、摘要、`docs/i18n/README.md`、`docs/i18n-coverage.md` 及 `docs/i18n-gap-backlog.md` |

## 完成檢查清單（強制）

合併前請驗證所有項目：

1. 語系導航同等性
- 根目錄語言切換列包含所有支援語系。
- 文件中心頁包含所有支援語系。
- 摘要語言項目包含所有支援語系。

2. 正式路徑一致性
- 非英文中心頁指向 `docs/i18n/<locale>/README.md`。
- 非英文摘要指向 `docs/i18n/<locale>/SUMMARY.md`。
- 相容性墊片不與正式項目矛盾。

3. 頂層文件同等性
- 若 `docs/*.md` 下的任何檔案變更，同步所有支援語系的在地化對應版本。
- 若同一 PR 中無法完成完整敘述翻譯，提供橋接更新（附原文連結）而非留下缺少的檔案。
- 橋接頁面必須包含原文章節導覽（至少 level-2 標題）與實用執行提示。

4. 覆蓋率中繼資料
- 若支援狀態、正式路徑或覆蓋層級有變更，更新 `docs/i18n-coverage.md`。
- 若基準線數量有變更，更新 `docs/i18n-gap-backlog.md`。
- 保持已變更在地化中心頁/摘要的日期戳記為最新。

5. 連結完整性
- 對已變更的文件執行 markdown/連結檢查（或等效的本地相對連結存在性檢查）。

## 延遲翻譯政策

若同一 PR 中無法完成完整敘述在地化：

- 保持檔案層級同等性完整（絕不留下缺少的語系檔案）。
- 使用在地化橋接頁面，附上指向英文規範文件的明確原文連結。
- 保持橋接頁面可操作：主題定位 + 原文章節導覽 + 執行提示。
- 在 PR 描述中加入明確的延遲說明，包含負責人與後續 issue/PR。

不得靜默延遲使用者可見的語言同等性變更。

## 代理程式工作流程規範

當代理程式涉及文件資訊架構或共用文件措辭時，代理程式必須：

1. 套用本指南，在同一 PR 中完成 i18n 跟進動作。
2. 當語系拓撲或同等性狀態變更時，更新 `docs/i18n-coverage.md`、`docs/i18n-gap-backlog.md` 及 `docs/i18n/README.md`。
3. 在 PR 摘要中包含 i18n 完成備註（已同步的內容、已橋接的內容、原因）。

## 缺口追蹤

- 在 [i18n-gap-backlog.md](i18n-gap-backlog.md) 中追蹤基準線同等性的關閉與重新開啟事件。
- 每次在地化波次後更新 [i18n-coverage.md](i18n-coverage.md)。

## 快速驗證指令

範例：

```bash
# 搜尋語系引用
rg -n "docs/i18n/(zh-CN|ja|ru|fr|vi|el)/README\.md|SUMMARY\.(zh-CN|ja|ru|fr|vi|el)\.md" README.md docs/SUMMARY*.md docs/i18n/*/README.md docs/i18n/*/SUMMARY.md

# 檢查已變更的 markdown 檔案
git status --short

# 快速同等性數量比對（對照頂層文件基準線）
base=$(find docs -maxdepth 1 -type f -name '*.md' | sed 's#^docs/##' | \
  rg -v '^(README(\..+)?\.md|SUMMARY(\..+)?\.md|commands-reference\.vi\.md|config-reference\.vi\.md|one-click-bootstrap\.vi\.md|troubleshooting\.vi\.md)$' | sort)
for loc in zh-CN ja ru fr vi el; do
  c=0
  while IFS= read -r f; do
    [ -f "docs/i18n/$loc/$f" ] || c=$((c+1))
  done <<< "$base"
  echo "$loc $c"
done
```

請使用專案偏好的 markdown lint/連結檢查工具。
