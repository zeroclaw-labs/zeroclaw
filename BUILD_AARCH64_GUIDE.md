# QuantClaw aarch64（树莓派）完整指南

本文件沉淀了本次实际排障与上线结果，目标是下次按文档可直接复现。

## 最终可用方案（已验证）

- 宿主机：Windows 11 + Docker Desktop
- 目标机：Raspberry Pi aarch64（512MB 可运行，建议按需加 swapfile）
- 服务名：`quantclaw-rust`（避免和已有 `quantclaw` 配网程序冲突）
- 运行根目录：`/home/<user>/quantclaw_rust_app`

## 一键流程

### 1) Windows 编译打包

在仓库根目录执行：

```powershell
.\scripts\build-release-aarch64.ps1
```

产物：

```text
dist\quantclaw-<version>-aarch64-linux-gnu.tar.gz
```

### 2) Windows 一键部署

```powershell
.\scripts\deploy-rpi.ps1 -RpiHost raspberrypi.local -RpiUser quant
```

可选：512MB 设备加 swapfile（不会改分区）：

```powershell
.\scripts\deploy-rpi.ps1 -RpiHost raspberrypi.local -RpiUser quant -EnsureSwap -SwapSizeMB 1024
```

### 3) 部署后验证

```bash
sudo systemctl status quantclaw-rust --no-pager
sudo ss -lntp | grep quantclaw
curl http://127.0.0.1:42617/health
```

### 4) 开机自启检查

```bash
sudo systemctl is-enabled quantclaw-rust
sudo systemctl enable --now quantclaw-rust
```

## 首次配置 API Key

默认 `.env` 在：

```text
/home/<user>/quantclaw_rust_app/.env
```

示例（OpenAI）：

```env
OPENAI_API_KEY=你的Key
```

如果使用 OpenRouter，请改成：

```env
OPENROUTER_API_KEY=你的Key
```

并确保 `config.toml` 里的 `default_provider` 对应一致，避免 key 前缀不匹配。

## 本次排障结论（过程记录）

### 构建链路

- 修复 OpenSSL 交叉编译路径（`pkg-config`/`OPENSSL_*`）；
- 修复 `Cargo.toml` 与 `Cargo.lock` 不一致导致 `--locked` 失败；
- 修复 Windows 下 `firmware` 链接退化为文本文件导致 Docker 构建报 `Not a directory`；
- 统一 aarch64 构建入口到 `Dockerfile.build-aarch64`。

### 部署链路

- 修复旧服务模板里 `gateway --host/--port` 参数不兼容当前 CLI；
- 改为 `ExecStart=/usr/local/bin/quantclaw gateway`，端口和 host 从 `config.toml` 读取；
- 服务改名为 `quantclaw-rust`，避免与旧 `quantclaw` 服务冲突；
- 运行目录固定为 `quantclaw_rust_app`，并采用 `releases/current` 结构便于升级回滚。

## 目录结构（Pi）

```text
/home/<user>/quantclaw_rust_app/
  |- releases/
  |   |- quantclaw-<version>-aarch64-linux-gnu/
  |- current -> releases/quantclaw-<version>-aarch64-linux-gnu
  |- .env
  |- .quantclaw/
      |- config.toml
      |- workspace/
```

## 常见故障速查

- `unexpected argument '--host'`：
  - 原因：旧服务模板；
  - 处理：使用 `quantclaw-rust.service` 模板并重载 systemd。

- `API key prefix mismatch`：
  - 原因：`default_provider` 与 `.env` key 类型不一致；
  - 处理：对齐 provider 和环境变量名。

- `curl 127.0.0.1:42617` 不通：
  - 原因：服务未起或参数错误；
  - 处理：先看 `journalctl -u quantclaw-rust -n 120 -l --no-pager`。
