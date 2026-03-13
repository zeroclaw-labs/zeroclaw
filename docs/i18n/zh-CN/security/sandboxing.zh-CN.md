# ZeroClaw 沙箱策略

> ⚠️ **状态：提案 / 路线图**
>
> 本文档描述提议的实现方法，可能包含假设的命令或配置。
> 如需了解当前运行时行为，请参见 [config-reference.zh-CN.md](../reference/api/config-reference.zh-CN.md)、[operations-runbook.zh-CN.md](../ops/operations-runbook.zh-CN.md) 和 [troubleshooting.zh-CN.md](../ops/troubleshooting.zh-CN.md)。

## 问题

ZeroClaw 当前具有应用层安全（白名单、路径阻止、命令注入保护），但缺少操作系统级别的 containment。如果攻击者在白名单中，他们可以使用 zeroclaw 的用户权限运行任何允许的命令。

## 提议的解决方案

### 选项 1：Firejail 集成（Linux 推荐）

Firejail 提供用户空间沙箱，开销极小。

```rust
// src/security/firejail.rs
use std::process::Command;

pub struct FirejailSandbox {
    enabled: bool,
}

impl FirejailSandbox {
    pub fn new() -> Self {
        let enabled = which::which(\"firejail\").is_ok();
        Self { enabled }
    }

    pub fn wrap_command(&self, cmd: &mut Command) -> &mut Command {
        if !self.enabled {
            return cmd;
        }

        // Firejail 使用沙箱包装任何命令
        let mut jail = Command::new(\"firejail\");
        jail.args([
            \"--private=home\",           // 新的 home 目录
            \"--private-dev\",            // 最小化 /dev
            \"--nosound\",                // 无音频
            \"--no3d\",                   // 无 3D 加速
            \"--novideo\",                // 无视频设备
            \"--nowheel\",                // 无输入设备
            \"--notv\",                   // 无 TV 设备
            \"--noprofile\",              // 跳过配置文件加载
            \"--quiet\",                  // 禁止警告
        ]);

        // 追加原始命令
        if let Some(program) = cmd.get_program().to_str() {
            jail.arg(program);
        }
        for arg in cmd.get_args() {
            if let Some(s) = arg.to_str() {
                jail.arg(s);
            }
        }

        // 用 firejail 包装替换原始命令
        *cmd = jail;
        cmd
    }
}
```

**配置选项：**
```toml
[security]
enable_sandbox = true
sandbox_backend = \"firejail\"  # 或 \"none\", \"bubblewrap\", \"docker\"
```

---

### 选项 2：Bubblewrap（便携，无需 root）

Bubblewrap 使用用户命名空间创建容器。

```bash
# 安装 bubblewrap
sudo apt install bubblewrap

# 包装命令：
bwrap --ro-bind /usr /usr \
      --dev /dev \
      --proc /proc \
      --bind /workspace /workspace \
      --unshare-all \
      --share-net \
      --die-with-parent \
      -- /bin/sh -c \"command\"
```

---

### 选项 3：Docker-in-Docker（重量级但完全隔离）

在临时容器中运行代理工具。

```rust
pub struct DockerSandbox {
    image: String,
}

impl DockerSandbox {
    pub async fn execute(&self, command: &str, workspace: &Path) -> Result<String> {
        let output = Command::new(\"docker\")
            .args([
                \"run\", \"--rm\",
                \"--memory\", \"512m\",
                \"--cpus\", \"1.0\",
                \"--network\", \"none\",
                \"--volume\", &format!(\"{}:/workspace\", workspace.display()),
                &self.image,
                \"sh\", \"-c\", command
            ])
            .output()
            .await?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}
```

---

### 选项 4：Landlock（Linux 内核 LSM，Rust 原生）

Landlock 提供文件系统访问控制，无需容器。

```rust
use landlock::{Ruleset, AccessFS};

pub fn apply_landlock() -> Result<()> {
    let ruleset = Ruleset::new()
        .set_access_fs(AccessFS::read_file | AccessFS::write_file)
        .add_path(Path::new(\"/workspace\"), AccessFS::read_file | AccessFS::write_file)?
        .add_path(Path::new(\"/tmp\"), AccessFS::read_file | AccessFS::write_file)?
        .restrict_self()?;

    Ok(())
}
```

---

## 实现优先级顺序

| 阶段 | 解决方案 | 工作量 | 安全收益 |
|-------|----------|--------|---------------|
| **P0** | Landlock（仅 Linux，原生） | 低 | 高（文件系统） |
| **P1** | Firejail 集成 | 低 | 极高 |
| **P2** | Bubblewrap 包装 | 中 | 极高 |
| **P3** | Docker 沙箱模式 | 高 | 完全 |

## 配置模式扩展

```toml
[security.sandbox]
enabled = true
backend = \"auto\"  # auto | firejail | bubblewrap | landlock | docker | none

# Firejail 特定配置
[security.sandbox.firejail]
extra_args = [\"--seccomp\", \"--caps.drop=all\"]

# Landlock 特定配置
[security.sandbox.landlock]
readonly_paths = [\"/usr\", \"/bin\", \"/lib\"]
readwrite_paths = [\"$HOME/workspace\", \"/tmp/zeroclaw\"]
```

## 测试策略

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn sandbox_blocks_path_traversal() {
        // 尝试通过沙箱读取 /etc/passwd
        let result = sandboxed_execute(\"cat /etc/passwd\");
        assert!(result.is_err());
    }

    #[test]
    fn sandbox_allows_workspace_access() {
        let result = sandboxed_execute(\"ls /workspace\");
        assert!(result.is_ok());
    }

    #[test]
    fn sandbox_no_network_isolation() {
        // 确保配置时网络被阻止
        let result = sandboxed_execute(\"curl http://example.com\");
        assert!(result.is_err());
    }
}
```
