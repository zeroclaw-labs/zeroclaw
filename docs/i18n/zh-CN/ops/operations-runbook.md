# ZeroClaw 运维操作手册

本操作手册适用于维护可用性、安全态势和事件响应的运维人员。

最后验证时间：**2026年2月18日**。

## 范围

本文档适用于日常运维操作：

- 启动和监管运行时
- 健康检查和诊断
- 安全发布和回滚
- 事件分类和恢复

首次安装请从 [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md) 开始。

## 运行时模式

| 模式 | 命令 | 使用场景 |
|---|---|---|
| 前台运行时 | `zeroclaw daemon` | 本地调试、短期会话 |
| 仅前台网关 | `zeroclaw gateway` | webhook 端点测试 |
| 用户服务 | `zeroclaw service install && zeroclaw service start` | 持久化运维管理的运行时 |
| Docker / Podman | `docker compose up -d` | 容器化部署 |

## Docker / Podman 运行时

如果你通过 `./install.sh --docker` 安装，容器会在引导完成后退出。要将 ZeroClaw 作为长效容器运行，请使用仓库中的 `docker-compose.yml`，或者针对持久化数据目录手动启动容器。

### 推荐：docker-compose

```bash
# 启动（后台运行，重启自动恢复）
docker compose up -d

# 停止
docker compose down

# 重启
docker compose up -d
```

如果使用 Podman，将 `docker` 替换为 `podman`。

### 手动容器生命周期管理

```bash
# 从引导镜像启动新容器
docker run -d --name zeroclaw \
  --restart unless-stopped \
  -v "$PWD/.zeroclaw-docker/.zeroclaw:/zeroclaw-data/.zeroclaw" \
  -v "$PWD/.zeroclaw-docker/workspace:/zeroclaw-data/workspace" \
  -e HOME=/zeroclaw-data \
  -e ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace \
  -p 42617:42617 \
  zeroclaw-bootstrap:local \
  gateway

# 停止（保留配置和工作区）
docker stop zeroclaw

# 重启已停止的容器
docker start zeroclaw

# 查看日志
docker logs -f zeroclaw

# 健康检查
docker exec zeroclaw zeroclaw status
```

对于 Podman，需要添加 `--userns keep-id --user "$(id -u):$(id -g)"` 并在卷挂载后添加 `:Z`。

### 关键要点：不要重新运行 install.sh 来重启

重新运行 `install.sh --docker` 会重建镜像并重新运行引导。要简单重启，只需要使用 `docker start`、`docker compose up -d` 或 `podman start`。

完整设置说明参见 [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md#stopping-and-restarting-a-dockerpodman-container)。

## 运维基线检查清单

1. 验证配置：

```bash
zeroclaw status
```

2. 验证诊断：

```bash
zeroclaw doctor
zeroclaw channel doctor
```

3. 启动运行时：

```bash
zeroclaw daemon
```

4. 对于持久化用户会话服务：

```bash
zeroclaw service install
zeroclaw service start
zeroclaw service status
```

## 健康和状态信号

| 信号 | 命令 / 文件 | 预期结果 |
|---|---|---|
| 配置有效性 | `zeroclaw doctor` | 无严重错误 |
| 渠道连通性 | `zeroclaw channel doctor` | 配置的渠道健康 |
| 运行时摘要 | `zeroclaw status` | 预期的提供商/模型/渠道 |
| 守护进程心跳/状态 | `~/.zeroclaw/daemon_state.json` | 文件定期更新 |

## 日志和诊断

### macOS / Windows（服务包装器日志）

- `~/.zeroclaw/logs/daemon.stdout.log`
- `~/.zeroclaw/logs/daemon.stderr.log`

### Linux（systemd 用户服务）

```bash
journalctl --user -u zeroclaw.service -f
```

## 事件分类流程（快速路径）

1. 快照系统状态：

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
```

2. 检查服务状态：

```bash
zeroclaw service status
```

3. 如果服务不健康，干净重启：

```bash
zeroclaw service stop
zeroclaw service start
```

4. 如果渠道仍然失败，验证 `~/.zeroclaw/config.toml` 中的白名单和凭证。

5. 如果涉及网关，验证绑定/认证设置（`[gateway]`）和本地可达性。

## 安全变更流程

应用配置更改前：

1. 备份 `~/.zeroclaw/config.toml`
2. 每次只应用一个逻辑变更
3. 运行 `zeroclaw doctor`
4. 重启守护进程/服务
5. 使用 `status` + `channel doctor` 验证

## 回滚流程

如果发布导致行为退化：

1. 恢复之前的 `config.toml`
2. 重启运行时（`daemon` 或 `service`）
3. 通过 `doctor` 和渠道健康检查确认恢复
4. 记录事件根本原因和缓解措施

## 相关文档

- [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md)
- [troubleshooting.md](./troubleshooting.md)
- [config-reference.md](../reference/api/config-reference.md)
- [commands-reference.md](../reference/cli/commands-reference.md)
