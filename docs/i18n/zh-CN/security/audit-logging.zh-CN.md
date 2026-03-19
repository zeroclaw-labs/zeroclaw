# JhedaiClaw 审计日志

> ⚠️ **状态：提案 / 路线图**
>
> 本文档描述提议的实现方法，可能包含假设的命令或配置。
> 如需了解当前运行时行为，请参见 [config-reference.zh-CN.md](../reference/api/config-reference.zh-CN.md)、[operations-runbook.zh-CN.md](../ops/operations-runbook.zh-CN.md) 和 [troubleshooting.zh-CN.md](../ops/troubleshooting.zh-CN.md)。

## 问题

JhedaiClaw 会记录操作，但缺乏防篡改审计追踪，用于记录：

- 谁执行了什么命令
- 何时以及从哪个渠道
- 访问了哪些资源
- 是否触发了安全策略

---

## 提议的审计日志格式

```json
{
  \"timestamp\": \"2026-02-16T12:34:56Z\",
  \"event_id\": \"evt_1a2b3c4d\",
  \"event_type\": \"command_execution\",
  \"actor\": {
    \"channel\": \"telegram\",
    \"user_id\": \"123456789\",
    \"username\": \"@alice\"
  },
  \"action\": {
    \"command\": \"ls -la\",
    \"risk_level\": \"low\",
    \"approved\": false,
    \"allowed\": true
  },
  \"result\": {
    \"success\": true,
    \"exit_code\": 0,
    \"duration_ms\": 15
  },
  \"security\": {
    \"policy_violation\": false,
    \"rate_limit_remaining\": 19
  },
  \"signature\": \"SHA256:abc123...\"  // 防篡改 HMAC 签名
}
```

---

## 实现

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

        // 如果配置了密钥则添加 HMAC 签名
        if let Some(ref key) = self.signing_key {
            let signature = compute_hmac(key, line.as_bytes());
            line.push_str(&format!(\"\\n\\\"signature\\\": \\\"{}\\\"\", signature));
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        writeln!(file, \"{}\", line)?;
        file.sync_all()?;  // 强制刷新确保持久化
        Ok(())
    }

    pub fn search(&self, filter: AuditFilter) -> Vec<AuditEvent> {
        // 按过滤条件搜索日志文件
        todo!()
    }
}
```

---

## 配置模式

```toml
[security.audit]
enabled = true
log_path = \"~/.config/jhedaiclaw/audit.log\"
max_size_mb = 100
rotate = \"daily\"  # daily | weekly | size

# 防篡改
sign_events = true
signing_key_path = \"~/.config/jhedaiclaw/audit.key\"

# 记录内容
log_commands = true
log_file_access = true
log_auth_events = true
log_policy_violations = true
```

---

## 审计查询 CLI

```bash
# 显示 @alice 执行的所有命令
jhedaiclaw audit --user @alice

# 显示所有高风险命令
jhedaiclaw audit --risk high

# 显示过去 24 小时的违规行为
jhedaiclaw audit --since 24h --violations-only

# 导出为 JSON 用于分析
jhedaiclaw audit --format json --output audit.json

# 验证日志完整性
jhedaiclaw audit --verify-signatures
```

---

## 日志轮转

```rust
pub fn rotate_audit_log(log_path: &PathBuf, max_size: u64) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(log_path)?;
    if metadata.len() < max_size {
        return Ok(());
    }

    // 轮转: audit.log -> audit.log.1 -> audit.log.2 -> ...
    let stem = log_path.file_stem().unwrap_or_default();
    let extension = log_path.extension().and_then(|s| s.to_str()).unwrap_or(\"log\");

    for i in (1..10).rev() {
        let old_name = format!(\"{}.{}.{}\", stem, i, extension);
        let new_name = format!(\"{}.{}.{}\", stem, i + 1, extension);
        let _ = std::fs::rename(old_name, new_name);
    }

    let rotated = format!(\"{}.1.{}\", stem, extension);
    std::fs::rename(log_path, &rotated)?;

    Ok(())
}
```

---

## 实现优先级

| 阶段   | 功能            | 工作量 | 安全价值 |
| ------ | --------------- | ------ | -------- |
| **P0** | 基础事件日志    | 低     | 中       |
| **P1** | 查询 CLI        | 中     | 中       |
| **P2** | HMAC 签名       | 中     | 高       |
| **P3** | 日志轮转 + 归档 | 低     | 中       |
