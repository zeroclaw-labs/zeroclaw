# ZeroClaw 沙箱策略（繁體中文）

> ⚠️ **狀態：提案 / 規劃路線**
>
> 本文件描述提案中的方法，可能包含假設性的指令或設定。
> 有關目前的執行期行為，請參閱 [config-reference.md](../../config-reference.md)、[operations-runbook.md](../../operations-runbook.md) 及 [troubleshooting.md](../../troubleshooting.md)。

## 問題

ZeroClaw 目前具備應用層安全防護（允許清單、路徑封鎖、指令注入防護），但缺乏作業系統層級的隔離機制。如果攻擊者位於允許清單中，他們可以用 ZeroClaw 的使用者權限執行任何已允許的指令。

## 提議方案

### 方案 1：Firejail 整合（Linux 推薦方案）

Firejail 提供使用者空間的沙箱機制，額外開銷極低。

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

        // Firejail 以沙箱機制包裝任意指令
        let mut jail = Command::new("firejail");
        jail.args([
            "--private=home",           // 新的家目錄
            "--private-dev",            // 最小化 /dev
            "--nosound",                // 無音訊
            "--no3d",                   // 無 3D 加速
            "--novideo",                // 無視訊裝置
            "--nowheel",                // 無輸入裝置
            "--notv",                   // 無電視裝置
            "--noprofile",              // 跳過設定檔載入
            "--quiet",                  // 抑制警告訊息
        ]);

        // 附加原始指令
        if let Some(program) = cmd.get_program().to_str() {
            jail.arg(program);
        }
        for arg in cmd.get_args() {
            if let Some(s) = arg.to_str() {
                jail.arg(s);
            }
        }

        // 將原始指令替換為 firejail 包裝版本
        *cmd = jail;
        cmd
    }
}
```

**設定選項：**
```toml
[security]
enable_sandbox = true
sandbox_backend = "firejail"  # 或 "none"、"bubblewrap"、"docker"
```

---

### 方案 2：Bubblewrap（可攜式，無需 root 權限）

Bubblewrap 使用使用者命名空間來建立容器。

```bash
# 安裝 bubblewrap
sudo apt install bubblewrap

# 包裝指令：
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

### 方案 3：Docker-in-Docker（重量級但完全隔離）

在臨時容器中執行代理工具。

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

### 方案 4：Landlock（Linux 核心 LSM，Rust 原生支援）

Landlock 提供不需要容器的檔案系統存取控制。

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

## 優先實作順序

| 階段 | 方案 | 工作量 | 安全性提升 |
|------|------|--------|-----------|
| **P0** | Landlock（僅限 Linux，原生支援） | 低 | 高（檔案系統層級） |
| **P1** | Firejail 整合 | 低 | 非常高 |
| **P2** | Bubblewrap 包裝器 | 中 | 非常高 |
| **P3** | Docker 沙箱模式 | 高 | 完整隔離 |

## 設定架構擴充

```toml
[security.sandbox]
enabled = true
backend = "auto"  # auto | firejail | bubblewrap | landlock | docker | none

# Firejail 專屬設定
[security.sandbox.firejail]
extra_args = ["--seccomp", "--caps.drop=all"]

# Landlock 專屬設定
[security.sandbox.landlock]
readonly_paths = ["/usr", "/bin", "/lib"]
readwrite_paths = ["$HOME/workspace", "/tmp/zeroclaw"]
```

## 測試策略

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn sandbox_blocks_path_traversal() {
        // 嘗試透過沙箱讀取 /etc/passwd
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
        // 確認在設定啟用時網路被封鎖
        let result = sandboxed_execute("curl http://example.com");
        assert!(result.is_err());
    }
}
```
