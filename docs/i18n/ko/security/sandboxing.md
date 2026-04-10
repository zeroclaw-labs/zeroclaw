# ZeroClaw Sandbox 전략

> ⚠️ **상태: 제안 / 로드맵**
>
> 이 문서는 제안된 접근 방식을 설명하며, 가상의 명령어나 설정을 포함할 수 있습니다.
> 현재 런타임 동작에 대해서는 [config-reference.md](../reference/api/config-reference.md), [operations-runbook.md](../ops/operations-runbook.md), [troubleshooting.md](../ops/troubleshooting.md)를 참고하십시오.

## 문제

ZeroClaw는 현재 애플리케이션 레이어 보안(allowlist, 경로 차단, 명령어 인젝션 보호)을 갖추고 있지만 OS 레벨 격리가 부족합니다. 공격자가 allowlist에 포함되어 있다면, ZeroClaw의 사용자 권한으로 허용된 모든 명령어를 실행할 수 있습니다.

## 제안된 솔루션

### 옵션 1: Firejail 통합 (Linux 권장)
Firejail은 최소한의 오버헤드로 사용자 공간 sandbox를 제공합니다.

```rust
// src/security/firejail.rs
use std::process::Command;

pub struct FirejailSandbox {
    enabled: bool,
}

impl FirejailSandbox {
    pub fn new() -> Self {
        let enabled = which::which("firejail").is_ok();
        Self { enabled }
    }

    pub fn wrap_command(&self, cmd: &mut Command) -> &mut Command {
        if !self.enabled {
            return cmd;
        }

        // Firejail이 모든 명령어를 sandbox로 래핑
        let mut jail = Command::new("firejail");
        jail.args([
            "--private=home",           // 새 홈 디렉토리
            "--private-dev",            // 최소 /dev
            "--nosound",                // 오디오 없음
            "--no3d",                   // 3D 가속 없음
            "--novideo",                // 비디오 장치 없음
            "--nowheel",                // 입력 장치 없음
            "--notv",                   // TV 장치 없음
            "--noprofile",              // 프로필 로딩 건너뛰기
            "--quiet",                  // 경고 억제
        ]);

        // 원본 명령어 추가
        if let Some(program) = cmd.get_program().to_str() {
            jail.arg(program);
        }
        for arg in cmd.get_args() {
            if let Some(s) = arg.to_str() {
                jail.arg(s);
            }
        }

        // 원본 명령어를 firejail 래퍼로 대체
        *cmd = jail;
        cmd
    }
}
```

**설정 옵션:**
```toml
[security]
enable_sandbox = true
sandbox_backend = "firejail"  # 또는 "none", "bubblewrap", "docker"
```

---

### 옵션 2: Bubblewrap (이식 가능, root 불필요)
Bubblewrap은 사용자 네임스페이스를 사용하여 컨테이너를 생성합니다.

```bash
# bubblewrap 설치
sudo apt install bubblewrap

# 명령어 래핑:
bwrap --ro-bind /usr /usr \
      --dev /dev \
      --proc /proc \
      --bind /workspace /workspace \
      --unshare-all \
      --share-net \
      --die-with-parent \
      -- /bin/sh -c "command"
```

---

### 옵션 3: Docker-in-Docker (무겁지만 완전한 격리)
에이전트 도구를 임시 컨테이너 내에서 실행합니다.

```rust
pub struct DockerSandbox {
    image: String,
}

impl DockerSandbox {
    pub async fn execute(&self, command: &str, workspace: &Path) -> Result<String> {
        let output = Command::new("docker")
            .args([
                "run", "--rm",
                "--memory", "512m",
                "--cpus", "1.0",
                "--network", "none",
                "--volume", &format!("{}:/workspace", workspace.display()),
                &self.image,
                "sh", "-c", command
            ])
            .output()
            .await?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}
```

---

### 옵션 4: Landlock (Linux 커널 LSM, Rust 네이티브)
Landlock은 컨테이너 없이 파일 시스템 접근 제어를 제공합니다.

```rust
use landlock::{Ruleset, AccessFS};

pub fn apply_landlock() -> Result<()> {
    let ruleset = Ruleset::new()
        .set_access_fs(AccessFS::read_file | AccessFS::write_file)
        .add_path(Path::new("/workspace"), AccessFS::read_file | AccessFS::write_file)?
        .add_path(Path::new("/tmp"), AccessFS::read_file | AccessFS::write_file)?
        .restrict_self()?;

    Ok(())
}
```

---

## 우선순위별 구현 순서

| 단계 | 솔루션 | 노력 | 보안 향상 |
|-------|----------|--------|---------------|
| **P0** | Landlock (Linux 전용, 네이티브) | 낮음 | 높음 (파일 시스템) |
| **P1** | Firejail 통합 | 낮음 | 매우 높음 |
| **P2** | Bubblewrap 래퍼 | 중간 | 매우 높음 |
| **P3** | Docker sandbox 모드 | 높음 | 완전 |

## 설정 스키마 확장

```toml
[security.sandbox]
enabled = true
backend = "auto"  # auto | firejail | bubblewrap | landlock | docker | none

# Firejail 전용
[security.sandbox.firejail]
extra_args = ["--seccomp", "--caps.drop=all"]

# Landlock 전용
[security.sandbox.landlock]
readonly_paths = ["/usr", "/bin", "/lib"]
readwrite_paths = ["$HOME/workspace", "/tmp/zeroclaw"]
```

## 테스트 전략

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn sandbox_blocks_path_traversal() {
        // sandbox를 통해 /etc/passwd 읽기 시도
        let result = sandboxed_execute("cat /etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn sandbox_allows_workspace_access() {
        let result = sandboxed_execute("ls /workspace");
        assert!(result.is_ok());
    }

    #[test]
    fn sandbox_no_network_isolation() {
        // 설정 시 네트워크가 차단되는지 확인
        let result = sandboxed_execute("curl http://example.com");
        assert!(result.is_err());
    }
}
```
