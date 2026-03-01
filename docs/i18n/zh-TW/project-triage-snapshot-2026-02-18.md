# ZeroClaw 專案分流快照（2026-02-18）（繁體中文）

截止日期：**2026 年 2 月 18 日**。

本快照擷取開放中的 PR/issue 訊號，用以引導文件與資訊架構工作。

## 資料來源

透過 GitHub CLI 從 `zeroclaw-labs/zeroclaw` 收集：

- `gh repo view ...`
- `gh pr list --state open --limit 500 ...`
- `gh issue list --state open --limit 500 ...`
- `gh pr/issue view <id> ...`（針對與文件相關的項目）

## 專案脈動

- 開放中 PR：**30**
- 開放中 Issue：**24**
- Stars：**11,220**
- Forks：**1,123**
- 預設分支：`main`
- GitHub API 授權中繼資料：`Other`（未偵測為 MIT）

## PR 標籤壓力（開放中 PR）

依頻率排列的主要訊號：

1. `risk: high` — 24
2. `experienced contributor` — 14
3. `size: S` — 14
4. `ci` — 11
5. `size: XS` — 10
6. `dependencies` — 7
7. `principal contributor` — 6

對文件的影響：

- CI/安全/服務變更仍為高異動區域。
- 面向運維人員的文件應優先呈現「變更了什麼」的能見度及快速疑難排解路徑。

## Issue 標籤壓力（開放中 Issue）

依頻率排列的主要訊號：

1. `experienced contributor` — 12
2. `enhancement` — 8
3. `bug` — 4

對文件的影響：

- 功能與效能需求仍超過說明性文件的產出速度。
- 疑難排解與操作參考應保持在頂層導航附近。

## 與文件相關的開放中 PR

- [#716](https://github.com/zeroclaw-labs/zeroclaw/pull/716) — OpenRC 支援（服務行為/文件影響）
- [#725](https://github.com/zeroclaw-labs/zeroclaw/pull/725) — shell 自動補全指令（CLI 文件影響）
- [#732](https://github.com/zeroclaw-labs/zeroclaw/pull/732) — CI action 替換（貢獻者工作流程文件影響）
- [#759](https://github.com/zeroclaw-labs/zeroclaw/pull/759) — daemon/channel 回應處理修正（channel 疑難排解影響）
- [#679](https://github.com/zeroclaw-labs/zeroclaw/pull/679) — 配對鎖定計數變更（安全行為文件影響）

## 與文件相關的開放中 Issue

- [#426](https://github.com/zeroclaw-labs/zeroclaw/issues/426) — 明確要求更清晰的功能文件
- [#666](https://github.com/zeroclaw-labs/zeroclaw/issues/666) — 操作手冊與告警/日誌指引需求
- [#745](https://github.com/zeroclaw-labs/zeroclaw/issues/745) — Docker pull 失敗（`ghcr.io`），顯示部署疑難排解需求
- [#761](https://github.com/zeroclaw-labs/zeroclaw/issues/761) — Armbian 編譯錯誤，凸顯平台疑難排解需求
- [#758](https://github.com/zeroclaw-labs/zeroclaw/issues/758) — 儲存後端彈性需求，影響設定/參考文件

## 建議的文件待辦事項（依優先順序）

1. **維持文件資訊架構穩定且明確**
   - 以 `docs/SUMMARY.md` + 分類索引作為正式導航。
   - 保持在地化中心頁與相同的頂層文件地圖對齊。

2. **保障運維人員的可發現性**
   - 在頂層 README/中心頁中保持 `operations-runbook` + `troubleshooting` 的連結。
   - 當問題重複出現時，新增平台專屬的疑難排解片段。

3. **積極追蹤 CLI/設定飄移**
   - 當涉及這些表面的 PR 合併時，更新 `commands/providers/channels/config` 參考文件。

4. **區分現行行為與提案**
   - 在安全路線圖文件中保留提案標示。
   - 明確標記執行期規範文件（`config/runbook/troubleshooting`）。

5. **維持快照紀律**
   - 快照須加註日期戳記且保持不可變。
   - 為每次文件衝刺建立新的快照檔案，而非修改歷史快照。

## 快照注意事項

本快照為特定時間點記錄（2026-02-18）。規劃新的文件衝刺前，請重新執行 `gh` 查詢。
