# ZeroClaw Audit Logging

> ⚠️ **상태: 제안 / 로드맵**
>
> 이 문서는 제안된 접근 방식을 설명하며, 가상의 명령어나 설정을 포함할 수 있습니다.
> 현재 런타임 동작에 대해서는 [config-reference.md](../reference/api/config-reference.md), [operations-runbook.md](../ops/operations-runbook.md), [troubleshooting.md](../ops/troubleshooting.md)를 참고하십시오.

## 문제

ZeroClaw는 작업을 로깅하지만, 다음에 대한 변조 방지 감사 추적이 부족합니다:
- 누가 어떤 명령어를 실행했는지
- 언제, 어떤 채널에서 실행했는지
- 어떤 리소스에 접근했는지
- 보안 정책이 트리거되었는지 여부

---

## 제안된 감사 로그 형식

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
  "signature": "SHA256:abc123..."  // 변조 방지를 위한 HMAC
}
```

---

## 구현

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

        // 키가 설정된 경우 HMAC 서명 추가
        if let Some(ref key) = self.signing_key {
            let signature = compute_hmac(key, line.as_bytes());
            line.push_str(&format!("\n\"signature\": \"{}\"", signature));
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        writeln!(file, "{}", line)?;
        file.sync_all()?;  // 내구성을 위해 강제 플러시
        Ok(())
    }

    pub fn search(&self, filter: AuditFilter) -> Vec<AuditEvent> {
        // 필터 기준으로 로그 파일 검색
        todo!()
    }
}
```

---

## 설정 스키마

```toml
[security.audit]
enabled = true
log_path = "~/.config/zeroclaw/audit.log"
max_size_mb = 100
rotate = "daily"  # daily | weekly | size

# 변조 방지
sign_events = true
signing_key_path = "~/.config/zeroclaw/audit.key"

# 로깅 대상
log_commands = true
log_file_access = true
log_auth_events = true
log_policy_violations = true
```

---

## 감사 쿼리 CLI

```bash
# @alice가 실행한 모든 명령어 표시
zeroclaw audit --user @alice

# 모든 고위험 명령어 표시
zeroclaw audit --risk high

# 최근 24시간의 위반 사항 표시
zeroclaw audit --since 24h --violations-only

# 분석을 위해 JSON으로 내보내기
zeroclaw audit --format json --output audit.json

# 로그 무결성 검증
zeroclaw audit --verify-signatures
```

---

## 로그 순환

```rust
pub fn rotate_audit_log(log_path: &PathBuf, max_size: u64) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(log_path)?;
    if metadata.len() < max_size {
        return Ok(());
    }

    // 순환: audit.log -> audit.log.1 -> audit.log.2 -> ...
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

## 구현 우선순위

| 단계 | 기능 | 노력 | 보안 가치 |
|-------|---------|--------|----------------|
| **P0** | 기본 이벤트 로깅 | 낮음 | 중간 |
| **P1** | 쿼리 CLI | 중간 | 중간 |
| **P2** | HMAC 서명 | 중간 | 높음 |
| **P3** | 로그 순환 + 보관 | 낮음 | 중간 |
