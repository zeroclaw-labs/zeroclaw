# One2X Fork 专属文档

本目录放 One2X fork（`one2x/custom-v*` 分支）专属的设计/调研/RFC 文档，不同步到上游 zeroclaw。

## 文件

| 文件 | 作用 | 面向 |
|------|------|------|
| `openclaw-architecture-final.md` | **主入口**：OpenClaw 架构合并版，含 DAG/Memory/Porting 总览 | 阅读起点 |
| `agent-intelligence-comparison.md` | OpenClaw / Hermes / ZeroClaw 三家对比（LCM 部分已修正） | 横向参考 |
| `porting-rfc.md` | LCM 机制移植到 ZeroClaw 的 RFC（P1/P2/P3 三阶段） | 开发实现 |
| `openclaw-deepdive.md` | OpenClaw 系统全景深度分析（16 章 + 2 附录） | 字典参考 |
| `openclaw-lcm-deepdive.md` | LCM（lossless-claw 插件）源码级分析 | 字典参考 |

## 阅读顺序

1. `openclaw-architecture-final.md` —— 先看这个，理清四个角色（记忆后端 / 上下文引擎 / 工具注入 / 召回子代理）
2. `agent-intelligence-comparison.md` —— 看三家差异，确认 ZeroClaw 缺什么
3. `porting-rfc.md` —— 按 P1 → P2 → P3 落地
4. 两个 deepdive —— 实现时遇到具体问题当字典查

## 背景

LCM（Lossless Context Management）是第三方插件 `@martian-engineering/lossless-claw`，不是 OpenClaw 内置。本系列文档的目标是把 LCM 的核心能力（可展开压缩、DAG 摘要、距离式召回）移植到 ZeroClaw Rust 代码库，填补当前 `[CONTEXT SUMMARY]` 压缩链的单向损失问题。

## 落地顺序

1. **P1（零风险）**: 检测到 `[CONTEXT SUMMARY]` 时动态注入 system prompt
2. **P2（中风险）**: 新增 `context_summary.rs` + summary→raw message SQLite 映射 + `context_expand` 工具
3. **P3（中高风险）**: 多级降级（normal/aggressive/fallback/capped）+ 子代理深度召回

详见 `porting-rfc.md`。

## 三个待决策问题

1. 新 SQLite DB 位置（并入主 memories DB vs 独立 sessions/<sid>/summaries.db）
2. `[CONTEXT SUMMARY sum_xxx]` 格式变更是否影响 zeroclaw-channels UI/日志
3. `context_deep_recall` 子代理模型配置（fast_model 复用 vs 独立配置）
