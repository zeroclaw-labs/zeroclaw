# 排程系統 (Cron & Scheduling)

ZeroClaw 內建功能完整的排程系統，可依排程、指定時間或固定間隔執行任務。

## 快速開始

```bash
# 新增 cron 排程（每天早上 9 點執行）
zeroclaw cron add '0 9 * * *' 'echo "Good morning!"'

# 新增一次性提醒（30 分鐘後執行）
zeroclaw cron once 30m 'notify-send "Time is up!"'

# 新增間隔排程（每 5 分鐘執行一次）
zeroclaw cron add-every 300000 'curl -s http://api.example.com/health'

# 列出所有排程任務
zeroclaw cron list

# 移除排程任務
zeroclaw cron remove <job-id>
```

## 排程類型

### Cron 運算式 (`kind: "cron"`)

標準 cron 運算式，支援可選的時區設定。

```bash
# 每個工作日太平洋時間早上 9 點
zeroclaw cron add '0 9 * * 1-5' --tz 'America/Los_Angeles' 'echo "Work time"'

# 每小時
zeroclaw cron add '0 * * * *' 'echo "Hourly check"'

# 每 15 分鐘
zeroclaw cron add '*/15 * * * *' 'curl http://localhost:8080/ping'
```

**格式：** `分鐘 小時 日 月 星期`

| 欄位 | 值 |
|------|------|
| 分鐘 | 0-59 |
| 小時 | 0-23 |
| 日 | 1-31 |
| 月 | 1-12 |
| 星期 | 0-6 (週日-週六) |

### 一次性 (`kind: "at"`)

在指定時間僅執行一次。

```bash
# 在指定的 ISO 時間戳
zeroclaw cron add-at '2026-03-15T14:30:00Z' 'echo "Meeting starts!"'

# 相對延遲（人性化格式）
zeroclaw cron once 2h 'echo "Two hours later"'
zeroclaw cron once 30m 'echo "Half hour reminder"'
zeroclaw cron once 1d 'echo "Tomorrow"'
```

**延遲單位：** `s`（秒）、`m`（分鐘）、`h`（小時）、`d`（天）

### 間隔 (`kind: "every"`)

以固定間隔重複執行。

```bash
# 每 5 分鐘（300000 毫秒）
zeroclaw cron add-every 300000 'echo "Ping"'

# 每小時（3600000 毫秒）
zeroclaw cron add-every 3600000 'curl http://api.example.com/sync'
```

## 任務類型

### Shell 任務

直接執行 shell 指令：

```bash
zeroclaw cron add '0 6 * * *' 'backup.sh && notify-send "Backup done"'
```

### Agent 任務

傳送提示詞給 AI 代理：

```toml
# 在 zeroclaw.toml 中
[[cron.jobs]]
schedule = { kind = "cron", expr = "0 9 * * *", tz = "America/Los_Angeles" }
job_type = "agent"
prompt = "Check my calendar and summarize today's events"
session_target = "main"  # 或 "isolated"
```

## 工作階段目標

控制 agent 任務的執行位置：

| 目標 | 行為 |
|------|------|
| `isolated`（預設） | 產生新的工作階段，無歷史紀錄 |
| `main` | 在主要工作階段中執行，擁有完整上下文 |

```toml
[[cron.jobs]]
schedule = { kind = "every", every_ms = 1800000 }  # 30 分鐘
job_type = "agent"
prompt = "Check for new emails and summarize any urgent ones"
session_target = "main"  # 可存取對話歷史
```

## 輸出傳遞設定

將任務輸出路由到頻道：

```toml
[[cron.jobs]]
schedule = { kind = "cron", expr = "0 8 * * *" }
job_type = "agent"
prompt = "Generate a morning briefing"
session_target = "isolated"

[cron.jobs.delivery]
mode = "channel"
channel = "telegram"
to = "123456789"  # Telegram 聊天 ID
best_effort = true  # 傳遞失敗時不中斷
```

**傳遞模式：**
- `none` - 不傳遞輸出（預設）
- `channel` - 傳送到指定頻道
- `notify` - 系統通知

## CLI 指令

| 指令 | 說明 |
|------|------|
| `zeroclaw cron list` | 顯示所有排程任務 |
| `zeroclaw cron add <expr> <cmd>` | 新增 cron 運算式任務 |
| `zeroclaw cron add-at <time> <cmd>` | 新增指定時間的一次性任務 |
| `zeroclaw cron add-every <ms> <cmd>` | 新增間隔任務 |
| `zeroclaw cron once <delay> <cmd>` | 新增延遲的一次性任務 |
| `zeroclaw cron update <id> [opts]` | 更新任務設定 |
| `zeroclaw cron remove <id>` | 刪除任務 |
| `zeroclaw cron pause <id>` | 暫停（停用）任務 |
| `zeroclaw cron resume <id>` | 恢復（啟用）任務 |

## 設定檔

在 `zeroclaw.toml` 中定義任務：

```toml
[[cron.jobs]]
name = "morning-briefing"
schedule = { kind = "cron", expr = "0 8 * * 1-5", tz = "America/New_York" }
job_type = "agent"
prompt = "Good morning! Check my calendar, emails, and weather."
session_target = "main"
enabled = true

[[cron.jobs]]
name = "health-check"
schedule = { kind = "every", every_ms = 60000 }
job_type = "shell"
command = "curl -sf http://localhost:8080/health || notify-send 'Service down!'"
enabled = true

[[cron.jobs]]
name = "daily-backup"
schedule = { kind = "cron", expr = "0 2 * * *" }
job_type = "shell"
command = "/home/user/scripts/backup.sh"
enabled = true
```

## 工具整合

排程系統也可作為 agent 工具使用：

| 工具 | 說明 |
|------|------|
| `cron_add` | 建立新的排程任務 |
| `cron_list` | 列出所有任務 |
| `cron_remove` | 刪除任務 |
| `cron_update` | 修改任務 |
| `cron_run` | 立即強制執行任務 |
| `cron_runs` | 顯示最近的執行歷史 |

### 範例：Agent 建立提醒

```
使用者：2 小時後提醒我打電話給媽媽
Agent：[使用 cron_add，kind="at"，delay="2h"]
完成！我會在下午 4:30 提醒您打電話給媽媽。
```

## 從 OpenClaw 遷移

ZeroClaw 的排程系統與 OpenClaw 的排程功能相容：

| OpenClaw | ZeroClaw |
|----------|----------|
| `kind: "cron"` | `kind = "cron"` ✅ |
| `kind: "every"` | `kind = "every"` ✅ |
| `kind: "at"` | `kind = "at"` ✅ |
| `sessionTarget: "main"` | `session_target = "main"` ✅ |
| `sessionTarget: "isolated"` | `session_target = "isolated"` ✅ |
| `payload.kind: "systemEvent"` | `job_type = "agent"` |
| `payload.kind: "agentTurn"` | `job_type = "agent"` |

**主要差異：** ZeroClaw 使用 TOML 設定格式，OpenClaw 使用 JSON。

## 最佳實務

1. **使用時區**來設定面向使用者的排程（會議、提醒）
2. **使用間隔**來設定背景任務（健康檢查、同步）
3. **使用一次性排程**來設定提醒和延遲動作
4. **設定 `session_target = "main"`** 當 agent 需要對話上下文時
5. **使用 `delivery`** 將輸出路由到正確的頻道

## 疑難排解

**任務未執行？**
- 檢查 `zeroclaw cron list` — 任務是否已啟用？
- 驗證 cron 運算式是否正確
- 檢查時區設定

**Agent 任務沒有上下文？**
- 將 `session_target` 從 `"isolated"` 改為 `"main"`

**輸出未傳遞？**
- 驗證 `delivery.channel` 是否已設定
- 檢查目標頻道是否已啟用
