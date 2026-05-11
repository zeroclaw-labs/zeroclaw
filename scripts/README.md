# scripts/ — Raspberry Pi 部署脚本说明

本目录脚本已统一为以下约定：

- 服务名固定使用 `quantclaw-rust`（避免与存量 `quantclaw` 配网服务冲突）
- 运行根目录固定为 `~/quantclaw_rust_app`
- 网关通过配置文件启动，不再在 systemd 里传 `--host/--port`

## 文件清单

| 文件 | 作用 |
|------|------|
| `build-release-aarch64.ps1` | Windows + Docker Desktop 打包脚本 |
| `build-release-aarch64.sh` | Linux/macOS 打包脚本 |
| `deploy-rpi.ps1` | Windows 一键部署脚本（含可选 swapfile） |
| `deploy-rpi.sh` | Linux/macOS 一键部署脚本 |
| `quantclaw-rust.service` | 默认 systemd 模板（目标服务名） |
| `rpi-config.toml` | 树莓派配置模板 |
| `99-act-led.rules` | ACT LED 权限规则 |

## 默认部署布局

| 路径 | 说明 |
|------|------|
| `~/quantclaw_rust_app/.env` | Provider 密钥等环境变量 |
| `~/quantclaw_rust_app/.quantclaw/config.toml` | 主配置 |
| `~/quantclaw_rust_app/current` | 当前运行目录（软链） |
| `/etc/systemd/system/quantclaw-rust.service` | 服务单元 |
| `/usr/local/bin/quantclaw` | 可执行文件 |

## Windows 推荐流程

```powershell
.\scripts\build-release-aarch64.ps1
.\scripts\deploy-rpi.ps1 -RpiHost raspberrypi.local -RpiUser quant
```

低内存设备可选：

```powershell
.\scripts\deploy-rpi.ps1 -RpiHost raspberrypi.local -RpiUser quant -EnsureSwap -SwapSizeMB 1024
```

`-EnsureSwap` 只管理 `/swapfile`，不会改分区表。

## Bash 推荐流程

```bash
RPI_HOST=raspberrypi.local RPI_USER=quant ./scripts/deploy-rpi.sh
```

可选参数：

- `RPI_DIR`：默认 `/home/$RPI_USER/quantclaw_rust_app`
- `SERVICE_NAME`：默认 `quantclaw-rust`
- `CROSS_TOOL`：`zigbuild` 或 `cross`

## 首次部署后

编辑密钥：

```bash
nano ~/quantclaw_rust_app/.env
```

可设置任一 Provider：

```env
OPENAI_API_KEY=
# 或
OPENROUTER_API_KEY=
```

检查服务：

```bash
sudo systemctl status quantclaw-rust --no-pager
curl http://127.0.0.1:42617/health
```

查看日志：

```bash
journalctl -u quantclaw-rust -f
```

## 历史排障记录

完整过程和结果沉淀在：

- `BUILD_AARCH64_GUIDE.md`
