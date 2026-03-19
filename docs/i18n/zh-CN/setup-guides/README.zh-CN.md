# 入门文档

适合首次设置和快速上手。

## 开始路径

1. 主概述和快速入门：[../../../../README.zh-CN.md](../../../../README.zh-CN.md)
2. 一键安装和双引导模式：[one-click-bootstrap.zh-CN.md](one-click-bootstrap.zh-CN.md)
3. macOS 上的更新或卸载：[macos-update-uninstall.zh-CN.md](macos-update-uninstall.zh-CN.md)
4. 按任务查找命令：[../reference/cli/commands-reference.zh-CN.md](../reference/cli/commands-reference.zh-CN.md)

## 选择你的路径

| 场景                        | 命令                                                                                     |
| --------------------------- | ---------------------------------------------------------------------------------------- |
| 我有 API 密钥，想要最快安装 | `jhedaiclaw onboard --api-key sk-... --provider openrouter`                              |
| 我想要引导式提示            | `jhedaiclaw onboard`                                                                     |
| 配置已存在，仅修复渠道配置  | `jhedaiclaw onboard --channels-only`                                                     |
| 配置已存在，我需要完全覆盖  | `jhedaiclaw onboard --force`                                                             |
| 使用订阅认证                | 查看 [订阅认证](../../../../README.zh-CN.md#subscription-auth-openai-codex--claude-code) |

## 引导和验证

- 快速引导：`jhedaiclaw onboard --api-key \"sk-...\" --provider openrouter`
- 引导式设置：`jhedaiclaw onboard`
- 现有配置保护：重新运行需要显式确认（非交互式流程中使用 `--force`）
- Ollama 云模型（`:cloud`）需要远程 `api_url` 和 API 密钥（例如 `api_url = \"https://ollama.com\"`）。
- 验证环境：`jhedaiclaw status` + `jhedaiclaw doctor`

## 下一步

- 运行时操作：[../ops/README.zh-CN.md](../ops/README.zh-CN.md)
- 参考目录：[../reference/README.zh-CN.md](../reference/README.zh-CN.md)
- macOS 生命周期任务：[macos-update-uninstall.zh-CN.md](macos-update-uninstall.zh-CN.md)
