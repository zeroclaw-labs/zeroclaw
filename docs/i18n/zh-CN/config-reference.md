# 配置参考（简体中文）

这是 Wave 1 首版本地化页面，用于查阅核心配置键、默认值与风险边界。

英文原文：

- [../../config-reference.md](../../config-reference.md)

## 适用场景

- 新环境初始化配置
- 排查配置项冲突与回退策略
- 审核安全相关配置与默认值

## 使用建议

- 配置键保持英文，避免本地化改写键名。
- 生产行为以英文原文定义为准。

## 更新说明（2026-03-03）

- `[agent]` 新增 `allowed_tools` 与 `denied_tools`：
  - `allowed_tools` 非空时，只向主代理暴露白名单工具。
  - `denied_tools` 在白名单过滤后继续移除工具。
- 未匹配的 `allowed_tools` 项会被跳过（调试日志提示），不会导致启动失败。
- 若同时配置 `allowed_tools` 与 `denied_tools` 且最终将可执行工具全部移除，启动会快速失败并给出明确错误。
- 详细字段表与示例见英文原文 `config-reference.md` 的 `[agent]` 小节。
