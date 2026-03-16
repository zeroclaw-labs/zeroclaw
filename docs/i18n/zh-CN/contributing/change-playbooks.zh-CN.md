# 变更操作手册

ZeroClaw 常见扩展和修改模式的分步指南。

每个扩展特征的完整代码示例请参见 [extension-examples.md](./extension-examples.zh-CN.md)。

## 添加提供商

- 在 `src/providers/` 中实现 `Provider` 特征。
- 在 `src/providers/mod.rs` 工厂中注册。
- 为工厂接线和错误路径添加聚焦测试。
- 避免提供商特定行为泄漏到共享编排代码中。

## 添加渠道

- 在 `src/channels/` 中实现 `Channel` 特征。
- 保持 `send`、`listen`、`health_check`、输入语义一致。
- 用测试覆盖认证/白名单/健康检查行为。

## 添加工具

- 在 `src/tools/` 中实现带有严格参数 schema 的 `Tool` 特征。
- 验证和清理所有输入。
- 返回结构化的 `ToolResult`；运行时路径中避免 panic。

## 添加外设

- 在 `src/peripherals/` 中实现 `Peripheral` 特征。
- 外设暴露 `tools()` —— 每个工具委托给硬件（GPIO、传感器等）。
- 如有需要，在配置 schema 中注册开发板类型。
- 协议和固件说明请参见 `docs/hardware/hardware-peripherals-design.md`。

## 安全/运行时/网关变更

- 包含威胁/风险说明和回滚策略。
- 为故障模式和边界添加/更新测试或验证证据。
- 保持可观测性有用但不包含敏感信息。
- 对于 `.github/workflows/**` 变更，在 PR 说明中包含 Actions 白名单影响，源变更时更新 `docs/contributing/actions-source-policy.md`。

## 文档系统/README/信息架构变更

- 将文档导航视为产品 UX：保持从 README → 文档中心 → SUMMARY → 分类索引的清晰路径。
- 保持顶层导航简洁；避免相邻导航块之间的重复链接。
- 运行时表面变更时，更新 `docs/reference/` 中的相关参考。
- 导航或关键措辞变更时，保持所有支持的语言（`en`、`zh-CN`、`ja`、`ru`、`fr`、`vi`）的多语言入口点一致。
- 共享文档措辞变更时，在同一个 PR 中同步对应的本地化文档（或显式记录延迟更新和后续 PR）。

## 架构边界规则

- 优先通过添加特征实现 + 工厂接线来扩展功能；避免为孤立功能进行跨模块重写。
- 保持依赖方向向内指向契约：具体集成依赖于特征/配置/工具层，而不是其他具体集成。
- 避免跨子系统耦合（例如提供商代码导入渠道内部实现，工具代码直接修改网关策略）。
- 保持模块职责单一：编排在 `agent/`、传输在 `channels/`、模型 I/O 在 `providers/`、策略在 `security/`、执行在 `tools/`。
- 仅在重复使用至少三次后（三原则）才引入新的共享抽象，且至少有一个真实调用者。
- 对于配置/schema 变更，将键视为公共契约：记录默认值、兼容性影响和迁移/回滚路径。
