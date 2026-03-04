# ZeroClaw i18n 覆蓋率與結構（繁體中文）

本文件定義 ZeroClaw 文件的在地化結構，並追蹤目前的覆蓋狀態。

最近更新日期：**2026 年 2 月 24 日**。

執行指南：[i18n-guide.md](i18n-guide.md)
缺口待辦清單：[i18n-gap-backlog.md](i18n-gap-backlog.md)

## 正式路徑配置

使用以下 i18n 路徑：

- 根目錄語言首頁：`README.md`（語言切換連結指向在地化中心頁）
- 完整在地化文件樹：`docs/i18n/<locale>/...`
- 選用相容性墊片（位於 docs 根目錄）：
  - `docs/SUMMARY.<locale>.md`
  - `docs/vi/**`

## 語系覆蓋率矩陣

| 語系 | 根目錄 README | 正式文件中心頁 | 指令參考 | 設定參考 | 疑難排解 | 狀態 |
|---|---|---|---|---|---|---|
| `en` | `README.md` | `docs/README.md` | `docs/commands-reference.md` | `docs/config-reference.md` | `docs/troubleshooting.md` | 權威來源 |
| `zh-CN` | `docs/i18n/zh-CN/README.md` | `docs/i18n/zh-CN/README.md` | `docs/i18n/zh-CN/commands-reference.md` | `docs/i18n/zh-CN/config-reference.md` | `docs/i18n/zh-CN/troubleshooting.md` | 完整頂層同等（橋接 + 在地化） |
| `zh-TW` | `docs/i18n/zh-TW/README.md` | `docs/i18n/zh-TW/README.md` | `docs/i18n/zh-TW/commands-reference.md` | `docs/i18n/zh-TW/config-reference.md` | `docs/i18n/zh-TW/troubleshooting.md` | 完整頂層同等（橋接 + 在地化） |
| `ja` | `docs/i18n/ja/README.md` | `docs/i18n/ja/README.md` | `docs/i18n/ja/commands-reference.md` | `docs/i18n/ja/config-reference.md` | `docs/i18n/ja/troubleshooting.md` | 完整頂層同等（橋接 + 在地化） |
| `ru` | `docs/i18n/ru/README.md` | `docs/i18n/ru/README.md` | `docs/i18n/ru/commands-reference.md` | `docs/i18n/ru/config-reference.md` | `docs/i18n/ru/troubleshooting.md` | 完整頂層同等（橋接 + 在地化） |
| `fr` | `docs/i18n/fr/README.md` | `docs/i18n/fr/README.md` | `docs/i18n/fr/commands-reference.md` | `docs/i18n/fr/config-reference.md` | `docs/i18n/fr/troubleshooting.md` | 完整頂層同等（橋接 + 在地化） |
| `vi` | `docs/i18n/vi/README.md` | `docs/i18n/vi/README.md` | `docs/i18n/vi/commands-reference.md` | `docs/i18n/vi/config-reference.md` | `docs/i18n/vi/troubleshooting.md` | 完整在地化樹 |
| `el` | `docs/i18n/el/README.md` | `docs/i18n/el/README.md` | `docs/i18n/el/commands-reference.md` | `docs/i18n/el/config-reference.md` | `docs/i18n/el/troubleshooting.md` | 完整在地化樹 |

## 頂層同等性快照

2026-02-24 基準線使用 40 份頂層英文文件（`docs/*.md`，排除語系根目錄變體）。

| 語系 | 缺少頂層同等數量 |
|---|---:|
| `zh-CN` | 0 |
| `zh-TW` | 0 |
| `ja` | 0 |
| `ru` | 0 |
| `fr` | 0 |
| `vi` | 0 |
| `el` | 0 |

## 敘述深度快照

截至 2026-02-24：

| 語系 | 增強型橋接頁面數 | 備註 |
|---|---:|---|
| `zh-CN` | 33 | 橋接頁面包含主題定位 + 原文章節導覽 + 執行提示 |
| `zh-TW` | 33 | 橋接頁面（透過 OpenCC s2twp + 台灣 IT 術語從 zh-CN 轉換） |
| `ja` | 33 | 橋接頁面包含主題定位 + 原文章節導覽 + 執行提示 |
| `ru` | 33 | 橋接頁面包含主題定位 + 原文章節導覽 + 執行提示 |
| `fr` | 33 | 橋接頁面包含主題定位 + 原文章節導覽 + 執行提示 |
| `vi` | 不適用 | 維持現有在地化風格作為完整在地化樹 |
| `el` | 不適用 | 維持現有在地化風格作為完整在地化樹 |

## 在地化首頁完整度

並非所有在地化首頁都是 `README.md` 的完整翻譯：

| 語系 | 風格 | 近似覆蓋率 |
|---|---|---|
| `en` | 完整原文 | 100% |
| `zh-CN` | 中心頁式進入點 | ~26% |
| `zh-TW` | 中心頁式進入點 | ~26% |
| `ja` | 中心頁式進入點 | ~26% |
| `ru` | 中心頁式進入點 | ~26% |
| `fr` | 接近完整翻譯 | ~90% |
| `vi` | 接近完整翻譯 | ~90% |
| `el` | 接近完整翻譯 | ~90% |

中心頁式進入點提供快速入門導引和語言導航，但不會複製英文 README 的全部內容。這是準確的狀態記錄，不代表需要立即解決的缺口。

對於 `zh-CN`、`ja`、`ru` 和 `fr`，正式 `docs/i18n/<locale>/` 中心頁已包含完整頂層同等覆蓋，並透過正式 i18n 路徑維持語言導航。

## 分類索引 i18n

所有支援語系現已在以下路徑建立在地化分類索引檔案：

- `docs/i18n/<locale>/getting-started/README.md`
- `docs/i18n/<locale>/reference/README.md`
- `docs/i18n/<locale>/operations/README.md`
- `docs/i18n/<locale>/security/README.md`
- `docs/i18n/<locale>/hardware/README.md`
- `docs/i18n/<locale>/contributing/README.md`
- `docs/i18n/<locale>/project/README.md`

分類索引在地化同等性已針對所有支援語系完成。

## 在地化規則

- 保持技術識別碼使用英文：
  - CLI 指令名稱
  - 設定鍵
  - API 路徑
  - trait/type 識別碼
- 優先採用簡潔、運維導向的在地化，而非逐字翻譯。
- 在地化頁面變更時更新「最近更新日期」/「最近同步日期」。
- 確保每個在地化中心頁都有「其他語言」區段。
- 遵循 [i18n-guide.md](i18n-guide.md) 了解強制完成與延遲政策。

## 新增語系

1. 在 `README.md` 語言切換中新增語系項目，指向 `docs/i18n/<locale>/README.md`。
2. 在 `docs/i18n/<locale>/` 下建立正式文件樹（至少包含 `README.md`、`commands-reference.md`、`config-reference.md`、`troubleshooting.md`）。
3. 在以下位置新增語系連結：
   - `docs/README.md` 中的在地化中心頁連結列
   - 每個 `docs/i18n/*/README.md` 的「其他語言」區段
   - `docs/SUMMARY.md`、`docs/i18n/*/SUMMARY.md` 以及 docs 根目錄的 `docs/SUMMARY.<locale>.md` 墊片（若存在）中的語言項目區段
4. 可選擇新增 docs 根目錄墊片檔案以維持向下相容。
5. 更新本檔案（`docs/i18n-coverage.md`）並執行連結驗證。

## 審查清單

- 所有在地化進入點檔案的連結均可正常解析。
- 無語系引用過時的檔名（例如 `README.vn.md`）。
- 目錄（`docs/SUMMARY.md`）與文件中心頁（`docs/README.md`）包含該語系。
