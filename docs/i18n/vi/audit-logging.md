# Audit logging

> ⚠️ **Trạng thái: Đề xuất / Lộ trình**
>
> Tài liệu này mô tả các hướng tiếp cận đề xuất và có thể bao gồm các lệnh hoặc cấu hình giả định.
> Để biết hành vi runtime hiện tại, xem [config-reference.md](config-reference.md), [operations-runbook.md](operations-runbook.md), và [troubleshooting.md](troubleshooting.md).

## Vấn đề

ZeroClaw ghi log các hành động nhưng thiếu audit trail chống giả mạo cho:
- Ai đã thực thi lệnh nào
- Khi nào và từ channel nào
- Những tài nguyên nào được truy cập
- Chính sách bảo mật có bị kích hoạt không

---

## Định dạng audit log đề xuất

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
  "signature": "SHA256:abc123..."  // HMAC để chống giả mạo
}
```

---

## Triển khai

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

        // Thêm chữ ký HMAC nếu key được cấu hình
        if let Some(ref key) = self.signing_key {
            let signature = compute_hmac(key, line.as_bytes());
            line.push_str(&format!("\n\"signature\": \"{}\"", signature));
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        writeln!(file, "{}", line)?;
        file.sync_all()?;  // Flush cưỡng bức để đảm bảo độ bền
        Ok(())
    }

    pub fn search(&self, filter: AuditFilter) -> Vec<AuditEvent> {
        // Tìm kiếm file log theo tiêu chí filter
        todo!()
    }
}
```

---

## Config schema

```toml
[security.audit]
enabled = true
log_path = "~/.config/zeroclaw/audit.log"
max_size_mb = 100
rotate = "daily"  # daily | weekly | size

# Chống giả mạo
sign_events = true
signing_key_path = "~/.config/zeroclaw/audit.key"

# Những gì cần log
log_commands = true
log_file_access = true
log_auth_events = true
log_policy_violations = true
```

---

## CLI truy vấn audit

```bash
# Hiển thị tất cả lệnh được thực thi bởi @alice
zeroclaw audit --user @alice

# Hiển thị tất cả lệnh rủi ro cao
zeroclaw audit --risk high

# Hiển thị vi phạm trong 24 giờ qua
zeroclaw audit --since 24h --violations-only

# Xuất sang JSON để phân tích
zeroclaw audit --format json --output audit.json

# Xác minh tính toàn vẹn của log
zeroclaw audit --verify-signatures
```

---

## Xoay vòng log

```rust
pub fn rotate_audit_log(log_path: &PathBuf, max_size: u64) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(log_path)?;
    if metadata.len() < max_size {
        return Ok(());
    }

    // Xoay vòng: audit.log -> audit.log.1 -> audit.log.2 -> ...
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

## Thứ tự triển khai

| Giai đoạn | Tính năng | Công sức | Giá trị bảo mật |
|-------|---------|--------|----------------|
| **P0** | Ghi log sự kiện cơ bản | Thấp | Trung bình |
| **P1** | Query CLI | Trung bình | Trung bình |
| **P2** | Ký HMAC | Trung bình | Cao |
| **P3** | Xoay vòng log + lưu trữ | Thấp | Trung bình |
