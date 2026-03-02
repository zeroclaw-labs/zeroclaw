# 命令参考（简体中文）

这是 Wave 1 首版本地化页面，用于快速定位 ZeroClaw CLI 命令。

英文原文：

- [../../commands-reference.md](../../commands-reference.md)

## 适用场景

- 按任务查命令（onboard / status / doctor / channel 等）
- 对比命令参数与行为边界
- 排查命令执行异常时确认预期输出

## 使用建议

- 命令名、参数名、配置键保持英文。
- 行为细节以英文原文为准。

## 最近更新

- `zeroclaw gateway` 新增 `--new-pairing` 参数，可清空已配对 token 并在网关启动时生成新的配对码。
- OpenClaw 迁移相关命令已加入英文原文：`zeroclaw onboard --migrate-openclaw`、`zeroclaw migrate openclaw`，并新增 agent 工具 `openclaw_migration`（本地化条目待补全，先以英文原文为准）。
