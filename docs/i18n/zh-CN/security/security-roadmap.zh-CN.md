# JhedaiClaw 安全改进路线图

> ⚠️ **状态：提案 / 路线图**
>
> 本文档描述提议的实现方法，可能包含假设的命令或配置。
> 如需了解当前运行时行为，请参见 [config-reference.zh-CN.md](../reference/api/config-reference.zh-CN.md)、[operations-runbook.zh-CN.md](../ops/operations-runbook.zh-CN.md) 和 [troubleshooting.zh-CN.md](../ops/troubleshooting.zh-CN.md)。

## 当前状态：坚实基础

JhedaiClaw 已经具备**出色的应用层安全**：

✅ 命令白名单（而非黑名单）
✅ 路径遍历保护
✅ 命令注入阻止（`$(...)`、反引号、`&&`、`>`）
✅ 密钥隔离（API 密钥不会泄露到 shell）
✅ 速率限制（每小时 20 个操作）
✅ 渠道授权（空 = 拒绝所有，`*` = 允许所有）
✅ 风险分类（低/中/高）
✅ 环境变量清理
✅ 禁止路径阻止
✅ 全面的测试覆盖（1,017 个测试）

## 缺失部分：操作系统级隔离

🔴 无操作系统级沙箱（chroot、容器、命名空间）
🔴 无资源限制（CPU、内存、磁盘 I/O 上限）
🔴 无防篡改审计日志
🔴 无系统调用过滤（seccomp）

---

## 对比：JhedaiClaw vs PicoClaw vs 生产级别

| 功能             | PicoClaw | 当前 JhedaiClaw | 路线图实现后的 JhedaiClaw | 生产目标         |
| ---------------- | -------- | --------------- | ------------------------- | ---------------- |
| **二进制大小**   | ~8MB     | **3.4MB** ✅    | 3.5-4MB                   | < 5MB            |
| **RAM 占用**     | < 10MB   | **< 5MB** ✅    | < 10MB                    | < 20MB           |
| **启动时间**     | < 1s     | **< 10ms** ✅   | < 50ms                    | < 100ms          |
| **命令白名单**   | 未知     | ✅ 是           | ✅ 是                     | ✅ 是            |
| **路径阻止**     | 未知     | ✅ 是           | ✅ 是                     | ✅ 是            |
| **注入保护**     | 未知     | ✅ 是           | ✅ 是                     | ✅ 是            |
| **操作系统沙箱** | 无       | ❌ 无           | ✅ Firejail/Landlock      | ✅ 容器/命名空间 |
| **资源限制**     | 无       | ❌ 无           | ✅ cgroups/监控           | ✅ 完整 cgroups  |
| **审计日志**     | 无       | ❌ 无           | ✅ HMAC 签名              | ✅ SIEM 集成     |
| **安全评分**     | C        | **B+**          | **A-**                    | **A+**           |

---

## 实现路线图

### 阶段 1：快速收益（1-2 周）

**目标：** 以最小复杂度解决关键缺口

| 任务                  | 文件                       | 工作量 | 影响 |
| --------------------- | -------------------------- | ------ | ---- |
| Landlock 文件系统沙箱 | `src/security/landlock.rs` | 2 天   | 高   |
| 内存监控 + OOM 终止   | `src/resources/memory.rs`  | 1 天   | 高   |
| 每个命令的 CPU 超时   | `src/tools/shell.rs`       | 1 天   | 高   |
| 基础审计日志          | `src/security/audit.rs`    | 2 天   | 中   |
| 配置模式更新          | `src/config/schema.rs`     | 1 天   | -    |

**交付成果：**

- Linux：文件系统访问限制在工作区范围内
- 所有平台：防止命令失控的内存/CPU 防护
- 所有平台：防篡改审计追踪

---

### 阶段 2：平台集成（2-3 周）

**目标：** 深度操作系统集成，实现生产级隔离

| 任务                                  | 工作量 | 影响 |
| ------------------------------------- | ------ | ---- |
| Firejail 自动检测 + 包装              | 3 天   | 极高 |
| 适用于 macOS/\*nix 的 Bubblewrap 包装 | 4 天   | 极高 |
| cgroups v2 systemd 集成               | 3 天   | 高   |
| seccomp 系统调用过滤                  | 5 天   | 高   |
| 审计日志查询 CLI                      | 2 天   | 中   |

**交付成果：**

- Linux：通过 Firejail 实现完整类容器隔离
- macOS：Bubblewrap 文件系统隔离
- Linux：cgroups 资源强制执行
- Linux：系统调用白名单

---

### 阶段 3：生产加固（1-2 周）

**目标：** 企业级安全功能

| 任务                                   | 工作量 | 影响 |
| -------------------------------------- | ------ | ---- |
| Docker 沙箱模式选项                    | 3 天   | 高   |
| 渠道的证书固定                         | 2 天   | 中   |
| 签名配置验证                           | 2 天   | 中   |
| 兼容 SIEM 的审计导出                   | 2 天   | 中   |
| 安全自检（`jhedaiclaw audit --check`） | 1 天   | 低   |

**交付成果：**

- 可选的基于 Docker 的执行隔离
- 渠道 webhook 的 HTTPS 证书固定
- 配置文件签名验证
- 用于外部分析的 JSON/CSV 审计导出

---

## 新配置模式预览

```toml
[security]
level = \"strict\"  # relaxed | default | strict | paranoid

# 沙箱配置
[security.sandbox]
enabled = true
backend = \"auto\"  # auto | firejail | bubblewrap | landlock | docker | none

# 资源限制
[resources]
max_memory_mb = 512
max_memory_per_command_mb = 128
max_cpu_percent = 50
max_cpu_time_seconds = 60
max_subprocesses = 10

# 审计日志
[security.audit]
enabled = true
log_path = \"~/.config/jhedaiclaw/audit.log\"
sign_events = true
max_size_mb = 100

# 自治（现有，增强）
[autonomy]
level = \"supervised\"  # readonly | supervised | full
allowed_commands = [\"git\", \"ls\", \"cat\", \"grep\", \"find\"]
forbidden_paths = [\"/etc\", \"/root\", \"~/.ssh\"]
require_approval_for_medium_risk = true
block_high_risk_commands = true
max_actions_per_hour = 20
```

---

## CLI 命令预览

```bash
# 安全状态检查
jhedaiclaw security --check
# → ✓ Sandbox: Firejail active
# → ✓ Audit logging enabled (42 events today)
# → → Resource limits: 512MB mem, 50% CPU

# 审计日志查询
jhedaiclaw audit --user @alice --since 24h
jhedaiclaw audit --risk high --violations-only
jhedaiclaw audit --verify-signatures

# 沙箱测试
jhedaiclaw sandbox --test
# → Testing isolation...
#   ✓ Cannot read /etc/passwd
#   ✓ Cannot access ~/.ssh
#   ✓ Can read /workspace
```

---

## 总结

**JhedaiClaw 已经比 PicoClaw 更安全**，具备：

- 小 50% 的二进制文件（3.4MB vs 8MB）
- 少 50% 的 RAM 占用（< 5MB vs < 10MB）
- 快 100 倍的启动速度（< 10ms vs < 1s）
- 全面的安全策略引擎
- 广泛的测试覆盖

**通过实现本路线图**，JhedaiClaw 将成为：

- 具备操作系统级沙箱的生产级产品
- 具备内存/CPU 防护的资源感知系统
- 具备防篡改日志的审计就绪系统
- 具备可配置安全级别的企业级产品

**预计工作量：** 完整实现需要 4-7 周
**价值：** 将 JhedaiClaw 从「适合测试」转变为「适合生产」
