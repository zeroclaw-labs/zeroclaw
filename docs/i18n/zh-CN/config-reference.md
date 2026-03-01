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

## 更新记录

- `runtime.reasoning_enabled` 现已支持 Qwen（所有别名，DashScope API），通过 `enable_thinking` 字段控制推理模式开关；原有 Ollama `think` 字段行为不变。详情见英文原文。
