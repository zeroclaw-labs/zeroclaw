# ZeroClaw 稽核日誌（繁體中文）

> ⚠️ **狀態：提案 / 規劃路線**
>
> 本文件描述提案中的方法，可能包含假設性的指令或設定。
> 有關目前的執行期行為，請參閱 [config-reference.md](../../config-reference.md)、[operations-runbook.md](../../operations-runbook.md) 及 [troubleshooting.md](../../troubleshooting.md)。

## 問題

ZeroClaw 會記錄動作，但缺乏防篡改的稽核軌跡來追蹤：
- 誰執行了什麼指令
- 何時以及從哪個頻道執行
- 存取了哪些資源
- 安全策略是否被觸發

---

## 提議的稽核日誌格式

```json
{
  "timestamp": "2026-02-16T12:34:56Z",
  "event_id": "evt_1a2b3c4d",
  "event_type": "command_execution",
  "actor": {
    "channel": "telegram",
    "user_id": "123456789",
    "username": "@alice"
  },
  "action": {
    "command": "ls -la",
    "risk_level": "low",
    "approved": false,
    "allowed": true
  },
  "result": {
    "success": true,
    "exit_code": 0,
    "duration_ms": 15
  },
  "security": {
    "policy_violation": false,
    "rate_limit_remaining": 19
  },
  "signature": "SHA256:abc123..."  // 用於防篡改的 HMAC 簽章
}
```

---

## 實作

```rust
// src/security/audit.rs
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: String,
    pub event_id: String,
    pub event_type: AuditEventType,
    pub actor: Actor,
    pub action: Action,
    pub result: ExecutionResult,
    pub security: SecurityContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditEventType {
    CommandExecution,
    FileAccess,
    ConfigurationChange,
    AuthSuccess,
    AuthFailure,
    PolicyViolation,
}

pub struct AuditLogger {
    log_path: PathBuf,
    signing_key: Option<hmac::Hmac<sha2::Sha256>>,
}

impl AuditLogger {
    pub fn log(&self, event: &AuditEvent) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(event)?;

        // 若已設定金鑰則加上 HMAC 簽章
        if let Some(ref key) = self.signing_key {
            let signature = compute_hmac(key, line.as_bytes());
            line.push_str(&format!("\n\"signature\": \"{}\"", signature));
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        writeln!(file, "{}", line)?;
        file.sync_all()?;  // 強制刷新以確保持久性
        Ok(())
    }

    pub fn search(&self, filter: AuditFilter) -> Vec<AuditEvent> {
        // 依篩選條件搜尋日誌檔
        todo!()
    }
}
```

---

## 設定架構

```toml
[security.audit]
enabled = true
log_path = "~/.config/zeroclaw/audit.log"
max_size_mb = 100
rotate = "daily"  # daily | weekly | size

# 防篡改機制
sign_events = true
signing_key_path = "~/.config/zeroclaw/audit.key"

# 記錄項目
log_commands = true
log_file_access = true
log_auth_events = true
log_policy_violations = true
```

---

## 稽核查詢 CLI

```bash
# 顯示 @alice 執行的所有指令
zeroclaw audit --user @alice

# 顯示所有高風險指令
zeroclaw audit --risk high

# 顯示過去 24 小時內的違規事件
zeroclaw audit --since 24h --violations-only

# 匯出為 JSON 供分析
zeroclaw audit --format json --output audit.json

# 驗證日誌完整性
zeroclaw audit --verify-signatures
```

---

## 日誌輪替

```rust
pub fn rotate_audit_log(log_path: &PathBuf, max_size: u64) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(log_path)?;
    if metadata.len() < max_size {
        return Ok(());
    }

    // 輪替：audit.log -> audit.log.1 -> audit.log.2 -> ...
    let stem = log_path.file_stem().unwrap_or_default();
    let extension = log_path.extension().and_then(|s| s.to_str()).unwrap_or("log");

    for i in (1..10).rev() {
        let old_name = format!("{}.{}.{}", stem, i, extension);
        let new_name = format!("{}.{}.{}", stem, i + 1, extension);
        let _ = std::fs::rename(old_name, new_name);
    }

    let rotated = format!("{}.1.{}", stem, extension);
    std::fs::rename(log_path, &rotated)?;

    Ok(())
}
```

---

## 實作優先順序

| 階段 | 功能 | 工作量 | 安全價值 |
|------|------|--------|---------|
| **P0** | 基礎事件記錄 | 低 | 中 |
| **P1** | 查詢 CLI | 中 | 中 |
| **P2** | HMAC 簽章 | 中 | 高 |
| **P3** | 日誌輪替 + 歸檔 | 低 | 中 |
