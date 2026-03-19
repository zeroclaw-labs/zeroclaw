# JhedaiClaw 运维操作手册

本操作手册适用于维护可用性、安全态势和事件响应的运维人员。

最后验证时间：**2026年2月18日**。

## 范围

本文档适用于日常运维操作：

- 启动和监管运行时
- 健康检查和诊断
- 安全发布和回滚
- 事件分类和恢复

首次安装请从 [one-click-bootstrap.zh-CN.md](../setup-guides/one-click-bootstrap.zh-CN.md) 开始。

## 运行时模式

| 模式       | 命令                                                     | 使用场景               |
| ---------- | -------------------------------------------------------- | ---------------------- |
| 前台运行时 | `jhedaiclaw daemon`                                      | 本地调试、短期会话     |
| 仅前台网关 | `jhedaiclaw gateway`                                     | webhook 端点测试       |
| 用户服务   | `jhedaiclaw service install && jhedaiclaw service start` | 持久化运维管理的运行时 |

## 运维基线检查清单

1. 验证配置：

```bash
jhedaiclaw status
```

2. 验证诊断：

```bash
jhedaiclaw doctor
jhedaiclaw channel doctor
```

3. 启动运行时：

```bash
jhedaiclaw daemon
```

4. 对于持久化用户会话服务：

```bash
jhedaiclaw service install
jhedaiclaw service start
jhedaiclaw service status
```

## 健康和状态信号

| 信号              | 命令 / 文件                       | 预期结果               |
| ----------------- | --------------------------------- | ---------------------- |
| 配置有效性        | `jhedaiclaw doctor`               | 无严重错误             |
| 渠道连通性        | `jhedaiclaw channel doctor`       | 配置的渠道健康         |
| 运行时摘要        | `jhedaiclaw status`               | 预期的提供商/模型/渠道 |
| 守护进程心跳/状态 | `~/.jhedaiclaw/daemon_state.json` | 文件定期更新           |

## 日志和诊断

### macOS / Windows（服务包装器日志）

- `~/.jhedaiclaw/logs/daemon.stdout.log`
- `~/.jhedaiclaw/logs/daemon.stderr.log`

### Linux（systemd 用户服务）

```bash
journalctl --user -u jhedaiclaw.service -f
```

## 事件分类流程（快速路径）

1. 快照系统状态：

```bash
jhedaiclaw status
jhedaiclaw doctor
jhedaiclaw channel doctor
```

2. 检查服务状态：

```bash
jhedaiclaw service status
```

3. 如果服务不健康，干净重启：

```bash
jhedaiclaw service stop
jhedaiclaw service start
```

4. 如果渠道仍然失败，验证 `~/.jhedaiclaw/config.toml` 中的白名单和凭证。

5. 如果涉及网关，验证绑定/认证设置（`[gateway]`）和本地可达性。

## 安全变更流程

应用配置更改前：

1. 备份 `~/.jhedaiclaw/config.toml`
2. 每次只应用一个逻辑变更
3. 运行 `jhedaiclaw doctor`
4. 重启守护进程/服务
5. 使用 `status` + `channel doctor` 验证

## 回滚流程

如果发布导致行为退化：

1. 恢复之前的 `config.toml`
2. 重启运行时（`daemon` 或 `service`）
3. 通过 `doctor` 和渠道健康检查确认恢复
4. 记录事件根本原因和缓解措施

## 相关文档

- [one-click-bootstrap.zh-CN.md](../setup-guides/one-click-bootstrap.zh-CN.md)
- [troubleshooting.zh-CN.md](./troubleshooting.zh-CN.md)
- [config-reference.zh-CN.md](../reference/api/config-reference.zh-CN.md)
- [commands-reference.zh-CN.md](../reference/cli/commands-reference.zh-CN.md)
